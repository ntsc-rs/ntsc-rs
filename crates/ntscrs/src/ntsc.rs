use core::{
    cmp,
    f32::consts::{FRAC_1_SQRT_2, PI},
    ops::Range,
};
use fearless_simd::{Level, dispatch, prelude::*};

#[cfg(not(feature = "std"))]
use core_maths::CoreFloat as _;

use crate::{
    ctx::Context,
    filter::TransferFunction,
    noise::{Fbm, Simplex, Simplex1d, Simplex2d, add_noise_1d, sample_noise_1d, sample_noise_2d},
    random::{SplitMix64, geometric_lambda},
    settings::standard::*,
    shift::{BoundaryHandling, shift_row, shift_row_to},
    thread_pool::{self, ZipChunks},
    yiq_fielding::{
        BlitInfo, Normalize, PixelFormat, YiqField, YiqOwned, YiqView, pixel_bytes_for,
    },
};

// 315/88 Mhz rate * 4
// TODO: why do we multiply by 4? composite-video-simulator does this for every filter and ntscqt defines NTSC_RATE the
// same way as we do here.
const NTSC_RATE: f32 = (315000000.00 / 88.0) * 4.0;

/// Create a simple constant-k lowpass filter with the given frequency cutoff, which can then be used to filter a signal.
fn make_lowpass(cutoff: f32, rate: f32) -> TransferFunction {
    let time_interval = 1.0 / rate;
    let tau = (cutoff * 2.0 * PI).recip();
    let alpha = time_interval / (tau + time_interval);

    TransferFunction::new(&[alpha], &[-(1.0 - alpha)])
}

/// Simulate three constant-k lowpass filters in a row by multiplying the coefficients. The original code
/// (composite-video-simulator and ntscqt) applies a lowpass filter 3 times in a row, but it's more efficient to
/// multiply the coefficients and just apply the filter once, which is mathematically equivalent.
fn make_lowpass_triple(cutoff: f32, rate: f32) -> TransferFunction {
    make_lowpass(cutoff, rate).cascade_self(3)
}

/// Construct a lowpass filter of the filter type given in the settings.
fn make_lowpass_for_type(cutoff: f32, rate: f32, filter_type: FilterType) -> TransferFunction {
    match filter_type {
        FilterType::ConstantK => make_lowpass_triple(cutoff, rate),
        FilterType::Butterworth => make_butterworth_filter(cutoff, rate),
    }
}

/// Create an IIR notch filter.
fn make_notch_filter(freq: f32, quality: f32) -> TransferFunction {
    // Adapted from scipy and simplified
    // https://github.com/scipy/scipy/blob/686422c4f0a71be1b4258309590fd3e9de102e18/scipy/signal/_filter_design.py#L5099-L5171
    if !(0.0..=1.0).contains(&freq) {
        panic!("Frequency outside valid range");
    }

    let bandwidth = (freq / quality) * PI;
    let freq = freq * PI;

    let beta = (bandwidth * 0.5).tan();

    let gain = (1.0 + beta).recip();

    let num = [gain, -2.0 * freq.cos() * gain, gain];
    let den = [-2.0 * freq.cos() * gain, 2.0 * gain - 1.0];

    TransferFunction::new(&num, &den)
}

/// Create a 2nd-order Butterworth filter.
fn make_butterworth_filter(cutoff: f32, rate: f32) -> TransferFunction {
    // Adapted from biquad-rs
    // https://github.com/korken89/biquad-rs/blob/aebd893a5c7e84ed1941b28b417cdbd1f3f530ae/src/coefficients.rs#L142
    let freq = (2.0 * cutoff).min(rate) / rate;
    let freq = freq * PI;
    let (omega_s, omega_c) = freq.sin_cos();
    let q_value = FRAC_1_SQRT_2;
    let alpha = omega_s / (2.0 * q_value);
    let gain = (1.0 + alpha).recip();

    TransferFunction::new(
        &[
            (1.0 - omega_c) * 0.5 * gain,
            (1.0 - omega_c) * gain,
            (1.0 - omega_c) * 0.5 * gain,
        ],
        &[-2.0 * omega_c * gain, (1.0 - alpha) * gain],
    )
}

/// The (low/high/whatever)pass filter's initial condition. The filter will start out assuming this steady-state value;
/// that is to say, they will assume that all the pixels to the left of where the image begins are set to this value.
#[allow(dead_code)]
enum InitialCondition {
    /// Convenience value--just use 0.
    Zero,
    /// Set the initial filter condition to a constant.
    Constant(f32),
    /// Set the initial filter condition to that of the first sample to be filtered.
    FirstSample,
}

/// We use a seeded RNG to generate random noise deterministically, but we don't want every pass which uses noise to use
/// the *same* noise. Each pass gets its own random seed which is mixed into the RNG.
mod noise_seeds {
    pub const VIDEO_COMPOSITE: u64 = 0;
    pub const HEAD_SWITCHING: u64 = 2;
    pub const TRACKING_NOISE: u64 = 3;
    pub const VIDEO_CHROMA_PHASE: u64 = 4;
    pub const EDGE_WAVE: u64 = 5;
    pub const SNOW: u64 = 6;
    pub const CHROMA_LOSS: u64 = 7;
    pub const HEAD_SWITCHING_MID_LINE_JITTER: u64 = 8;

    pub const VIDEO_CHROMA_I: u64 = 1;
    pub const VIDEO_CHROMA_Q: u64 = 9;
    pub const VIDEO_LUMA: u64 = 10;
}

/// Common settings/info that many passes need to use. Passed to each individual effect function as needed.
struct EffectCtx {
    /// SIMD feature level token.
    level: fearless_simd::Level,
    /// Random seed.
    seed: u64,
    /// Current frame index.
    frame_num: usize,
    /// The "bandwidth scale" setting, used mainly for IIR filters.
    horizontal_scale: f32,
    /// The "vertical scale" setting.
    vertical_scale: f32,
}

struct FixedQueue<T, const N: usize> {
    queue: [T; N],
    i: usize,
}
impl<T, const N: usize> FixedQueue<T, N> {
    #[inline(always)]
    fn new(items: [T; N]) -> Self {
        Self { queue: items, i: 0 }
    }

    #[inline(always)]
    fn push(&mut self, item: T) -> T {
        let res = core::mem::replace(&mut self.queue[self.i], item);
        self.i = (self.i + 1) % N;
        res
    }

    #[inline(always)]
    fn queue(&self) -> &[T; N] {
        &self.queue
    }
}

impl EffectCtx {
    fn new(
        ctx: &Context,
        settings: &NtscEffect,
        height: usize,
        frame_num: usize,
        scale_factor: [f32; 2],
    ) -> Self {
        let video_size_scale_factor =
            if settings.scale.enabled && settings.scale.settings.scale_with_video_size {
                height as f32 / 480.0
            } else {
                1.0
            };

        Self {
            level: ctx.level,
            seed: settings.random_seed as u32 as u64,
            frame_num,
            horizontal_scale: scale_factor[0]
                * video_size_scale_factor
                * settings
                    .scale
                    .as_option()
                    .map(|scale| scale.horizontal_scale)
                    .unwrap_or(1.0),
            vertical_scale: scale_factor[1]
                * video_size_scale_factor
                * settings
                    .scale
                    .as_option()
                    .map(|scale| scale.vertical_scale)
                    .unwrap_or(1.0),
        }
    }

    /// Apply a given IIR filter to each row of one color plane.
    /// # Arguments
    /// - `plane` - The entire data (width * height) for the plane to be filtered.
    /// - `width` - The width of each row.
    /// - `filter` - The filter to apply to the plane.
    /// - `initial` - The initial steady-state value of the filter.
    /// - `scale` - Scale the effect of the filter on the signal by this amount.
    /// - `delay` - Offset the filter output backwards (to the left) by this amount.
    fn filter_plane(
        &self,
        plane: &mut [f32],
        width: usize,
        filter: &TransferFunction,
        initial: InitialCondition,
        delay: usize,
    ) {
        if filter.should_use_simd(self.level) {
            // The optimal number of rows seems to vary by architecture.
            #[cfg(target_family = "wasm")]
            self.filter_plane_with_rows::<6>(plane, width, filter, initial, delay);
            #[cfg(not(target_family = "wasm"))]
            self.filter_plane_with_rows::<8>(plane, width, filter, initial, delay);
        } else {
            self.filter_plane_with_rows::<1>(plane, width, filter, initial, delay);
        }
    }

    fn filter_plane_with_rows<const ROWS: usize>(
        &self,
        plane: &mut [f32],
        width: usize,
        filter: &TransferFunction,
        initial: InitialCondition,
        delay: usize,
    ) {
        let row_chunks = ZipChunks::new([plane], width * ROWS);

        // Process the row in chunks, each `ROWS` rows tall.
        row_chunks.par_for_each(|_, [rows]| {
            if rows.len() != width * ROWS {
                // Handling the remainder
                for row in rows.chunks_exact_mut(width) {
                    let initial = match initial {
                        InitialCondition::Zero => 0.0,
                        InitialCondition::Constant(c) => c,
                        InitialCondition::FirstSample => row[0],
                    };
                    filter.filter_signal_in_place::<1>(self.level, &mut [row], [initial], delay);
                }
                return;
            }

            let mut iter = rows.chunks_exact_mut(width);
            let mut rows: [&mut [f32]; ROWS] = core::array::from_fn(|_| iter.next().unwrap());

            let initial_rows: [f32; ROWS] = match initial {
                InitialCondition::Zero => [0.0f32; ROWS],
                InitialCondition::Constant(c) => [c; ROWS],
                InitialCondition::FirstSample => core::array::from_fn(|i| rows[i][0]),
            };

            filter.filter_signal_in_place::<ROWS>(self.level, &mut rows, initial_rows, delay);
        });
    }

    /// Apply a lowpass filter to the luminance (Y) plane before the Y, I, and Q planes are modulated together. This should
    /// reduce color fringing/rainbow artifacts, which may or may not be what you want. Note that this is *not* the "Luma
    /// smear" setting--that's its own function (`luma_smear`).
    fn luma_filter(&self, frame: &mut YiqView, filter_mode: LumaLowpass) {
        match filter_mode {
            LumaLowpass::None => {}
            LumaLowpass::Box => {
                let width = frame.dimensions.0;
                if width < 2 {
                    return;
                }
                ZipChunks::new([frame.y], width).par_for_each(|_, [y]| {
                    let mut delay = FixedQueue::new([16.0 / 255.0, 16.0 / 255.0, y[0], y[1]]);
                    let mut sum: f32 = delay.queue().iter().sum();

                    // Box-blur the signal.
                    for index in 0..width - 2 {
                        let c = unsafe { *y.get_unchecked(index + 2) };
                        sum -= delay.push(c);
                        sum += c;
                        y[index] = sum * 0.25;
                    }
                    // Handle the remainder separately
                    let last = y[width - 1];
                    for index in width - 2..width {
                        let c = last;
                        sum -= delay.push(c);
                        sum += c;
                        y[index] = sum * 0.25;
                    }
                });
            }
            LumaLowpass::Notch => self.filter_plane(
                frame.y,
                frame.dimensions.0,
                &make_notch_filter(0.5, 2.0),
                InitialCondition::FirstSample,
                0,
            ),
        }
    }

    /// Apply a lowpass filter to the input chroma, emulating broadcast NTSC's bandwidth cutoffs.
    /// (Well, almost--Wikipedia (https://en.wikipedia.org/wiki/YIQ) puts the Q bandwidth at 0.4 MHz, not 0.6. Although
    /// that statement seems unsourced and I can't find any info on it...)
    fn composite_chroma_lowpass(&self, frame: &mut YiqView, filter_type: FilterType) {
        let i_filter =
            make_lowpass_for_type(1300000.0, NTSC_RATE * self.horizontal_scale, filter_type);
        let q_filter =
            make_lowpass_for_type(600000.0, NTSC_RATE * self.horizontal_scale, filter_type);

        let width = frame.dimensions.0;

        thread_pool::join(
            || self.filter_plane(frame.i, width, &i_filter, InitialCondition::Zero, 2),
            || self.filter_plane(frame.q, width, &q_filter, InitialCondition::Zero, 4),
        );
    }

    /// Apply a less intense lowpass filter to the input chroma.
    fn composite_chroma_lowpass_lite(&self, frame: &mut YiqView, filter_type: FilterType) {
        let filter =
            make_lowpass_for_type(2600000.0, NTSC_RATE * self.horizontal_scale, filter_type);

        let width = frame.dimensions.0;

        thread_pool::join(
            || self.filter_plane(frame.i, width, &filter, InitialCondition::Zero, 1),
            || self.filter_plane(frame.q, width, &filter, InitialCondition::Zero, 1),
        );
    }

    /// Calculate the chroma subcarrier phase for a given row/field.
    fn chroma_phase_shift(
        &self,
        scanline_phase_shift: PhaseShift,
        offset: i32,
        line_num: usize,
    ) -> usize {
        match scanline_phase_shift {
            PhaseShift::Degrees90 | PhaseShift::Degrees270 => {
                ((self.frame_num as i32 + offset + ((line_num as i32) >> 1)) & 3) as usize
            }
            PhaseShift::Degrees180 => {
                ((((self.frame_num + line_num) & 2) as i32 + offset) & 3) as usize
            }
            PhaseShift::Degrees0 => 0,
        }
    }

    /// Modulate a single line of the chrominance signal into the luminance signal.
    fn chroma_into_luma_line(y: &mut [f32], i: &mut [f32], q: &mut [f32], xi: usize) {
        const I_MULT: [f32; 4] = [1.0, 0.0, -1.0, 0.0];
        const Q_MULT: [f32; 4] = [0.0, 1.0, 0.0, -1.0];

        y.iter_mut()
            .zip(i.iter_mut().zip(q))
            .enumerate()
            .for_each(|(index, (y, (i, q)))| {
                let phase = (index + (xi & 3)) & 3;
                *y += *i * I_MULT[phase] + *q * Q_MULT[phase];
                // *i = 0.0;
                // *q = 0.0;
            });
    }

    /// Modulate the chrominance signal (I and Q planes) into the Y (luminance) plane.
    /// TODO: Make the chroma carrier's frequency/sample rate configurable.
    fn chroma_into_luma(&self, yiq: &mut YiqView, phase_shift: PhaseShift, phase_offset: i32) {
        let width = yiq.dimensions.0;

        let yiq_lines = ZipChunks::new([yiq.y, yiq.i, yiq.q], width);

        yiq_lines.par_for_each(|index, [y, i, q]| {
            let xi = self.chroma_phase_shift(phase_shift, phase_offset, index * 2);

            Self::chroma_into_luma_line(y, i, q, xi);
        });
    }

    /// Demodulate the chroma back into the I and Q channels, given the Y channel and the modulated signal. This inner loop
    /// uses SIMD on 4-wide chunks at a time and doesn't handle boundary conditions. The very first column, and any
    /// remainder afterwards, is handled by the non-SIMD function.
    #[inline(always)]
    fn demodulate_chroma_simd_inner<S: Simd>(
        simd: S,
        y: &[f32],
        i: &mut [f32],
        q: &mut [f32],
        modulated: &[f32],
        xi: usize,
    ) -> usize {
        let width = y.len();

        let offset_wave = |offset: usize| {
            S::f32s::from_fn(simd, |i| {
                let ii = offset + i;

                let sign = if ii & 2 == 0 { -1.0 } else { 1.0 };
                let mag = if ii & 1 == 0 { 1.0 } else { 0.0 };
                sign * mag
            })
        };
        let i_mult_inv_l = offset_wave(xi) * 0.5;
        let i_mult_inv_c = offset_wave(1 + xi);
        let i_mult_inv_r = offset_wave(2 + xi) * 0.5;
        let q_mult_inv_l = offset_wave(3 + xi) * 0.5;
        let q_mult_inv_c = offset_wave(4 + xi);
        let q_mult_inv_r = offset_wave(5 + xi) * 0.5;

        let mut index = 1;
        while index < width.saturating_sub(S::f32s::N) {
            let yy_l = S::f32s::from_slice(simd, &y[index - 1..index - 1 + S::f32s::N]);
            let yy_c = S::f32s::from_slice(simd, &y[index..index + S::f32s::N]);
            let yy_r = S::f32s::from_slice(simd, &y[index + 1..index + 1 + S::f32s::N]);
            let mm_l = S::f32s::from_slice(simd, &modulated[index - 1..index - 1 + S::f32s::N]);
            let mm_c = S::f32s::from_slice(simd, &modulated[index..index + S::f32s::N]);
            let mm_r = S::f32s::from_slice(simd, &modulated[index + 1..index + 1 + S::f32s::N]);
            let chroma_l = yy_l - mm_l;
            let chroma_c = yy_c - mm_c;
            let chroma_r = yy_r - mm_r;

            let mut i_modulated = chroma_c * i_mult_inv_c;
            i_modulated = chroma_l.mul_add(i_mult_inv_l, i_modulated);
            i_modulated = chroma_r.mul_add(i_mult_inv_r, i_modulated);
            let mut q_modulated = chroma_c * q_mult_inv_c;
            q_modulated = chroma_l.mul_add(q_mult_inv_l, q_modulated);
            q_modulated = chroma_r.mul_add(q_mult_inv_r, q_modulated);

            i[index..index + S::f32s::N].copy_from_slice(i_modulated.as_slice());
            q[index..index + S::f32s::N].copy_from_slice(q_modulated.as_slice());

            index += S::f32s::N;
        }

        index
    }

    fn demodulate_chroma_simd(
        y: &[f32],
        i: &mut [f32],
        q: &mut [f32],
        modulated: &[f32],
        xi: usize,
        level: Level,
    ) -> usize {
        dispatch!(level, simd => Self::demodulate_chroma_simd_inner(simd, y, i, q, modulated, xi))
    }

    /// Demodulate the chrominance (I and Q) signals from a combined NTSC signal, looking at a single source pixel at the
    /// given index and writing into the destination I and Q plane pixels as well as their immediate neighbors.
    fn demodulate_chroma_line(
        &self,
        y: &[f32],
        i: &mut [f32],
        q: &mut [f32],
        modulated: &[f32],
        xi: usize,
    ) {
        assert_eq!(y.len(), modulated.len());
        assert_eq!(y.len(), i.len());
        assert_eq!(y.len(), q.len());
        let width = y.len();

        // The SIMD loop doesn't handle boundary conditions and operates on chunks of 4 pixels at a time. We need to handle
        // both the leftmost pixel and the rightmost few. We unconditionally handle the leftmost one, so the "rightmost"
        // range starts at 1. If there is no SIMD, process everything using the scalar approach.
        let remainder_start = if self.level.is_fallback() {
            1
        } else {
            Self::demodulate_chroma_simd(y, i, q, modulated, xi, self.level).max(1)
        };

        const I_MULT_INV: [f32; 4] = [-1.0, 0.0, 1.0, 0.0];
        const Q_MULT_INV: [f32; 4] = [0.0, -1.0, 0.0, 1.0];

        for index in core::iter::once(0).chain(remainder_start..width) {
            let offset_c = (index + xi) & 3;
            let chroma_c = y[index] - modulated[index];
            let mut i_modulated = chroma_c * I_MULT_INV[offset_c];
            let mut q_modulated = chroma_c * Q_MULT_INV[offset_c];

            if index < width - 1 {
                let offset_r = (index.wrapping_add(1) + xi) & 3;
                let chroma_r = y[index + 1] - modulated[index + 1];
                i_modulated += chroma_r * I_MULT_INV[offset_r] * 0.5;
                q_modulated += chroma_r * Q_MULT_INV[offset_r] * 0.5;
            }
            if index > 0 {
                let offset_l = (index.wrapping_sub(1) + xi) & 3;
                let chroma_l = y[index - 1] - modulated[index - 1];
                i_modulated += chroma_l * I_MULT_INV[offset_l] * 0.5;
                q_modulated += chroma_l * Q_MULT_INV[offset_l] * 0.5;
            }
            i[index] = i_modulated;
            q[index] = q_modulated;
        }
    }

    /// Demodulate the chrominance signal using a box filter to separate it out.
    fn luma_into_chroma_line_box(
        &self,
        y: &mut [f32],
        i: &mut [f32],
        q: &mut [f32],
        modulated: &[f32],
        xi: usize,
    ) {
        let width = y.len();
        for index in 0..width {
            let area = [
                modulated
                    .get(index.wrapping_sub(1))
                    .cloned()
                    .unwrap_or(16.0 / 255.0),
                modulated[index],
                modulated[(index + 1).min(width - 1)],
                modulated[(index + 2).min(width - 1)],
            ];
            y[index] = area.iter().sum::<f32>() * 0.25;
        }
        self.demodulate_chroma_line(y, i, q, modulated, xi);
    }

    /// Demodulate the chroma signal from the Y (luma) plane back into the I and Q planes.
    /// TODO: Make the chroma carrier's frequency/sample rate configurable.
    fn luma_into_chroma(
        &self,
        yiq: &mut YiqView,
        mut filter_mode: ChromaDemodulationFilter,
        phase_shift: PhaseShift,
        phase_offset: i32,
    ) {
        let width = yiq.dimensions.0;
        let height = yiq.num_rows();

        // For all four demodulation methods, we copy the original modulated signal to the scratch buffer, then write the
        // demodulated Y signal back into yiq.y
        let modulated = &mut yiq.scratch;
        modulated.copy_from_slice(yiq.y);

        // The comb filters only work if we have at least 2 lines. To avoid a panic, use a notch filter if we have only 1 line.
        if height == 1
            && matches!(
                filter_mode,
                ChromaDemodulationFilter::OneLineComb | ChromaDemodulationFilter::TwoLineComb
            )
        {
            filter_mode = ChromaDemodulationFilter::Notch;
        }

        match filter_mode {
            ChromaDemodulationFilter::Box => {
                let lines = ZipChunks::new([yiq.y, yiq.i, yiq.q, modulated], width);
                lines.par_for_each(|index, [y, i, q, modulated]| {
                    let xi = self.chroma_phase_shift(phase_shift, phase_offset, index * 2);

                    self.luma_into_chroma_line_box(y, i, q, modulated, xi);
                });
            }
            ChromaDemodulationFilter::Notch => {
                // Apply a notch filter to the signal to remove the high-frequency chroma carrier, and store it in the
                // scratch buffer. We can then get *just* the chroma by subtracting the filtered signal from the original.
                let filter: TransferFunction = make_notch_filter(0.5, 2.0);
                self.filter_plane(yiq.y, width, &filter, InitialCondition::Zero, 0);

                let lines = ZipChunks::new([yiq.y, yiq.i, yiq.q, modulated], width);
                lines.par_for_each(|index, [y, i, q, modulated]| {
                    let xi = self.chroma_phase_shift(phase_shift, phase_offset, index * 2);
                    self.demodulate_chroma_line(y, i, q, modulated, xi);
                });
            }
            ChromaDemodulationFilter::OneLineComb => {
                // Demodulate the Y (luma) by averaging successive lines of the modulated signal
                ZipChunks::new([yiq.y, yiq.i, yiq.q], width).par_for_each(
                    |line_index, [y, i, q]| {
                        // "Reflect" line 2 to line 0, so that the chroma is properly demodulated for line 0.
                        // A comb filter requires the phase of the chroma carrier to alternate per line, so simply repeating line 1
                        // wouldn't work.
                        let top_line = if line_index == 0 {
                            &modulated[width..width * 2]
                        } else {
                            &modulated[(line_index - 1) * width..line_index * width]
                        };
                        let bottom_line = &modulated[line_index * width..(line_index + 1) * width];
                        // Average the two lines
                        for (y, (&top, &bottom)) in
                            y.iter_mut().zip(top_line.iter().zip(bottom_line))
                        {
                            *y = (top + bottom) * 0.5;
                        }

                        // Demodulate the chroma
                        let xi = self.chroma_phase_shift(phase_shift, phase_offset, line_index * 2);
                        self.demodulate_chroma_line(y, i, q, bottom_line, xi);
                    },
                );
            }
            ChromaDemodulationFilter::TwoLineComb => {
            	let mut y_notch = yiq.y.to_vec();
            	let notch_filter: TransferFunction = make_notch_filter(0.5, 2.0);
            	self.filter_plane(&mut y_notch, width, &notch_filter, InitialCondition::Zero, 0);
            	
                let lines = ZipChunks::new([yiq.y, yiq.i, yiq.q], width);
                lines.par_for_each(|line_index, [y, i, q]| {
                    // For the first line, both prev_line and next_line point to the second line. This effectively makes
                    // it a one-line comb filter for that line. See the comment above in the one-line comb filter for
                    // why we do this.
                    let prev_index = if line_index == 0 { 1 } else { line_index - 1 };

                    // Similar for the last line.
                    let next_index = if line_index == height - 1 {
                        height - 2
                    } else {
                        line_index + 1
                    };

                    let prev_line = &modulated[prev_index * width..(prev_index + 1) * width];
                    let cur_line = &modulated[line_index * width..(line_index + 1) * width];
                    let next_line = &modulated[next_index * width..(next_index + 1) * width];

                    for sample_index in 0..width {
                    	let prev_sample = prev_line[sample_index];
                        let cur_sample = cur_line[sample_index];
                        let next_sample = next_line[sample_index];

                        let prev_diff = (cur_sample - prev_sample).abs();
                        let next_diff = (cur_sample - next_sample).abs();
                        
                        let blended = (cur_sample * 0.5)
                            + (prev_sample * 0.25)
                            + (next_sample * 0.25);
                        
                        y[sample_index] = if prev_diff < 0.03 && prev_diff < next_diff {
                        	(cur_sample + prev_sample) * 0.5
                        } else if next_diff < 0.03 && next_diff < prev_diff {
                        	(cur_sample + next_sample) * 0.5
                        } else {
                        	y_notch[line_index * width + sample_index]
                        };
                    }

                    let xi = self.chroma_phase_shift(phase_shift, phase_offset, line_index * 2);
                    self.demodulate_chroma_line(y, i, q, cur_line, xi);
                });
            }
        };
    }

    /// Blur the luminance plane using a lowpass filter.
    fn luma_smear(&self, yiq: &mut YiqView, amount: f32) {
        let lowpass = make_lowpass(f32::exp2(-4.0 * amount) * 0.25, self.horizontal_scale);
        self.filter_plane(yiq.y, yiq.dimensions.0, &lowpass, InitialCondition::Zero, 0);
    }

    /// Helper function to apply gradient noise to a single row of a single plane.
    fn video_noise_line(
        &self,
        row: &mut [f32],
        rng: &SplitMix64,
        index: usize,
        frequency: f32,
        intensity: f32,
        detail: u32,
    ) {
        let width = row.len();
        let mut rng = rng.clone().mix(index as u64);
        let noise_seed = rng.random::<i32>();
        let offset = rng.random::<f32>() * width as f32;

        let noise = Fbm {
            seed: noise_seed,
            octaves: detail.clamp(1, 5) as usize,
            gain: 1.0,
            lacunarity: 2.0,
            frequency,
        };

        add_noise_1d::<Simplex1d, _>(self.level, &noise, intensity * 0.25, [offset], [width], row);
    }

    /// Add gradient noise to an NTSC-encoded (composite) signal.
    fn composite_noise(&self, yiq: &mut YiqView, noise_settings: &FbmNoiseSettings) {
        let width = yiq.dimensions.0;
        let rng = SplitMix64::new(self.seed)
            .mix(noise_seeds::VIDEO_COMPOSITE)
            .mix(self.frame_num as u64);

        ZipChunks::new([yiq.y], width).par_for_each(|index, [row]| {
            self.video_noise_line(
                row,
                &rng,
                index,
                noise_settings.frequency / self.horizontal_scale,
                noise_settings.intensity,
                noise_settings.detail.try_into().unwrap_or_default(),
            );
        });
    }

    /// Add gradient noise to a single plane of a de-modulated signal.
    fn plane_noise(
        &self,
        plane: &mut [f32],
        width: usize,
        settings: &FbmNoiseSettings,
        noise_seed: u64,
    ) {
        let rng = SplitMix64::new(self.seed)
            .mix(noise_seed)
            .mix(self.frame_num as u64);

        ZipChunks::new([plane], width).par_for_each(|index, [row]| {
            self.video_noise_line(
                row,
                &rng,
                index,
                settings.frequency / self.horizontal_scale,
                settings.intensity,
                settings.detail.try_into().unwrap_or_default(),
            );
        });
    }

    /// Emulate timing error in the chrominance carrier for a single line.
    fn chroma_phase_offset_line(i: &mut [f32], q: &mut [f32], offset: f32) {
        // Phase shift angle in radians. Mapped so that an intensity of 1.0 is a phase shift ranging from a full
        // rotation to the left, to a full rotation to the right.
        let phase_shift = offset * PI * 2.0;
        let (sin_angle, cos_angle) = phase_shift.sin_cos();

        for (i, q) in i.iter_mut().zip(q.iter_mut()) {
            // Treat (i, q) as a 2D vector and rotate it by the phase shift amount.
            let rotated_i = (*i * cos_angle) - (*q * sin_angle);
            let rotated_q = (*i * sin_angle) + (*q * cos_angle);

            *i = rotated_i;
            *q = rotated_q;
        }
    }

    /// Emulate timing error in the chrominance carrier, which results in a hue shift.
    fn chroma_phase_error(&self, yiq: &mut YiqView, intensity: f32) {
        let width = yiq.dimensions.0;

        ZipChunks::new([yiq.i, yiq.q], width).par_for_each(|_, [i, q]| {
            Self::chroma_phase_offset_line(i, q, intensity);
        });
    }

    /// Add per-scanline chroma phase error.
    fn chroma_phase_noise(&self, yiq: &mut YiqView, intensity: f32) {
        let width = yiq.dimensions.0;
        let rng = SplitMix64::new(self.seed)
            .mix(noise_seeds::VIDEO_CHROMA_PHASE)
            .mix(self.frame_num as u64);

        ZipChunks::new([yiq.i, yiq.q], width).par_for_each(|index, [i, q]| {
            // Phase shift angle in radians. Mapped so that an intensity of 1.0 is a phase shift ranging from a full
            // rotation to the left, to a full rotation to the right.
            let phase_shift =
                (rng.clone().mix(index as u64).random::<f32>() - 0.5) * 2.0 * intensity;

            Self::chroma_phase_offset_line(i, q, phase_shift);
        });
    }

    /// Emulate VHS head-switching at the bottom of the image.
    fn head_switching(
        &self,
        yiq: &mut YiqView,
        num_rows: usize,
        offset: usize,
        shift: f32,
        mid_line: Option<&HeadSwitchingMidLineSettings>,
    ) {
        let num_rows = (num_rows as f32 * self.vertical_scale).round() as usize;
        let offset = (offset as f32 * self.vertical_scale).round() as usize;
        if offset > num_rows {
            return;
        }
        let num_affected_rows = num_rows - offset;

        let width = yiq.dimensions.0;
        let height = yiq.num_rows();
        // Handle cases where the number of affected rows exceeds the number of actual rows in the image
        let start_row = height.max(num_affected_rows) - num_affected_rows;
        let affected_rows = &mut yiq.y[start_row * width..];
        let scratch = &mut yiq.scratch[start_row * width..];
        let cut_off_rows = num_affected_rows.saturating_sub(height);

        let rng = SplitMix64::new(self.seed)
            .mix(noise_seeds::HEAD_SWITCHING)
            .mix(self.frame_num as u64);

        ZipChunks::new([affected_rows, scratch], width).par_for_each(|index, [row, scratch]| {
            let index = num_affected_rows - (index + cut_off_rows);
            let row_shift = shift * ((index + offset) as f32 / num_rows as f32).powf(1.5);
            let noisy_shift = (row_shift + (rng.clone().mix(index as u64).random::<f32>() - 0.5))
                * self.horizontal_scale;

            if index == num_affected_rows
                && let Some(mid_line) = mid_line
            {
                // Shift the entire row, but only copy back a portion of it.
                shift_row_to(
                    row,
                    scratch,
                    noisy_shift,
                    BoundaryHandling::Constant(0.0),
                    self.level,
                );

                let mut rng = SplitMix64::new(self.seed)
                    .mix(noise_seeds::HEAD_SWITCHING_MID_LINE_JITTER)
                    .mix(self.frame_num as u64);

                // Average two random numbers to bias the result towards the middle
                let jitter_rand = (rng.random::<f32>() + rng.random::<f32>()) * 0.5;
                let jitter = (jitter_rand - 0.5) * mid_line.jitter;

                let copy_start = (width as f32 * (mid_line.position + jitter)) as usize;
                if copy_start > width {
                    return;
                }
                row[copy_start..].copy_from_slice(&scratch[copy_start..]);

                // Add a transient where the head switch is supposed to start
                let transient_intensity = (rng.random::<f32>() + 0.5) * 0.5;
                let transient_len = 16.0 * self.horizontal_scale;

                for i in copy_start..(copy_start + transient_len.ceil() as usize).min(width) {
                    let x = (i - copy_start) as f32;
                    row[i] += (1.0 - (x / transient_len)).powi(3) * transient_intensity;
                }
            } else {
                shift_row(
                    row,
                    noisy_shift,
                    BoundaryHandling::Constant(0.0),
                    self.level,
                );
            }
        });
    }

    /// Helper function for generating "snow"/transient speckles.
    fn row_speckles(&self, row: &mut [f32], rng: &mut SplitMix64, intensity: f32, anisotropy: f32) {
        let intensity = intensity as f64;
        let anisotropy = anisotropy as f64;
        const TRANSIENT_LEN_RANGE: Range<f32> = 8.0..64.0;

        // Anisotropy controls how much the snow appears "clumped" within given lines vs. appearing independently across
        // lines.
        //
        // Transition smoothly from a flat function that always returns `intensity`, to a step function that returns
        // 1.0 with a probability of `intensity` and 0.0 with a probability of `1.0 - intensity`. In-between states
        // look like S-curves with increasing sharpness.
        // As a bonus, the integral of this function over (0, 1) as we transition from 0% to 100% anisotropy is *almost*
        // constant, meaning there's approximately the same amount of snow each time.
        let logistic_factor = ((rng.random::<f64>() - intensity)
            / (intensity * (1.0 - intensity) * (1.0 - anisotropy)))
            .exp();
        // Intensity of the "snow" for this specific line
        let mut line_snow_intensity =
            anisotropy / (1.0 + logistic_factor) + intensity * (1.0 - anisotropy);

        // At maximum intensity (1.0), the line just gets completely whited out. 0.125 intensity looks more reasonable.
        line_snow_intensity *= 0.125;
        line_snow_intensity = line_snow_intensity.clamp(0.0, 1.0);
        if line_snow_intensity <= 0.0 {
            return;
        }

        // Turn each pixel into "snow" with probability snow_intensity * intensity_scale
        // We can simulate the distance between each "snow" pixel with a geometric distribution which avoids having to
        // loop over every pixel:
        // https://en.wikipedia.org/wiki/Geometric_distribution
        let dist = geometric_lambda(line_snow_intensity);
        // Start leftwards of the visible region to simulate transients that may have started before it. This avoids
        // transients being sparser towards the leftmost edge.
        let mut pixel_idx = (-TRANSIENT_LEN_RANGE.end).floor() as isize;
        loop {
            pixel_idx += rng.random_geometric(dist).min(isize::MAX as usize) as isize;
            if pixel_idx >= row.len() as isize {
                break;
            }

            let transient_len: f32 = rng.random_range(TRANSIENT_LEN_RANGE) * self.horizontal_scale;
            let transient_freq = rng.random_range(transient_len * 3.0..transient_len * 5.0);
            let pixel_idx_end = pixel_idx + transient_len.ceil() as isize;

            // Each transient gets its own RNG to determine the intensity of each pixel within it.
            // This is to prevent the length of each transient from affecting the random state of the subsequent
            // transient, which can cause the snow to "jitter" when changing the "bandwidth scale" setting.
            let mut transient_rng = SplitMix64::new(rng.random());

            for i in
                pixel_idx.clamp(0, row.len() as isize)..pixel_idx_end.clamp(0, row.len() as isize)
            {
                let x = (i - pixel_idx) as f32;
                // Simulate transient with sin(pi*x / 4) * (1 - x/len)^2
                row[i as usize] += ((x * PI) / transient_freq).cos()
                    * (1.0f32 - x / transient_len).powi(2)
                    * transient_rng.random_range(-1.0..2.0);
            }

            // Make sure we advance the pixel index each time. Our geometric distribution gives us the time between
            // successive events, which can be 0 for very high probabilities.
            pixel_idx += 1;
        }
    }

    /// Emulate VHS tracking error/noise.
    /// TODO: this is inaccurate and doesn't let you control the position of the noise.
    /// I need to revamp this at some point.
    fn tracking_noise(
        &self,
        yiq: &mut YiqView,
        num_rows: usize,
        wave_intensity: f32,
        snow_intensity: f32,
        snow_anisotropy: f32,
        noise_intensity: f32,
    ) {
        let num_rows = (num_rows as f32 * self.vertical_scale).round() as usize;
        let width = yiq.dimensions.0;
        let height = yiq.num_rows();

        let mut rng = SplitMix64::new(self.seed)
            .mix(noise_seeds::TRACKING_NOISE)
            .mix(self.frame_num as u64);
        let noise_seed = rng.random::<i32>();
        let offset = rng.random::<f32>() * yiq.num_rows() as f32;

        // Handle cases where the number of affected rows exceeds the number of actual rows in the image
        let start_row = height.max(num_rows) - num_rows;
        let cut_off_rows = num_rows.saturating_sub(height);
        let affected_rows = &mut yiq.y[start_row * width..];

        let shift_noise = &mut yiq.scratch[0..num_rows.min(height)];
        let noise = Simplex {
            seed: noise_seed,
            frequency: 0.5,
        };
        sample_noise_1d::<Simplex1d, _>(
            self.level,
            &noise,
            [offset],
            [num_rows.min(height)],
            shift_noise,
        );

        ZipChunks::new([affected_rows], width).par_for_each(|index, [row]| {
            let index = index + cut_off_rows;
            // This iterates from the top down. Increase the intensity as we approach the bottom of the picture.
            let intensity_scale = index as f32 / num_rows as f32;
            shift_row(
                row,
                shift_noise[index - cut_off_rows]
                    * intensity_scale
                    * wave_intensity
                    * 0.25
                    * self.horizontal_scale,
                BoundaryHandling::Constant(0.0),
                self.level,
            );

            self.video_noise_line(
                row,
                &rng,
                index,
                0.25 / self.horizontal_scale,
                intensity_scale.powi(2) * noise_intensity * 4.0,
                1,
            );

            self.row_speckles(
                row,
                &mut rng.clone().mix(index as u64),
                snow_intensity * intensity_scale.powi(2),
                snow_anisotropy,
            );
        });
    }

    /// Add random bits of "snow" to an NTSC-encoded signal.
    fn snow(&self, yiq: &mut YiqView, intensity: f32, anisotropy: f32) {
        let rng = SplitMix64::new(self.seed)
            .mix(noise_seeds::SNOW)
            .mix(self.frame_num as u64);

        ZipChunks::new([yiq.y], yiq.dimensions.0).par_for_each(|index, [row]| {
            self.row_speckles(
                row,
                &mut rng.clone().mix(index as u64),
                intensity,
                anisotropy,
            );
        });
    }

    /// Offset the chrominance (I and Q) planes horizontally and/or vertically.
    /// Note how the horizontal shift is a float (the signal is continuous), but the vertical shift is an int (each scanline
    /// is discrete).
    fn chroma_delay(&self, yiq: &mut YiqView, offset: (f32, isize)) {
        let offset = (
            offset.0 * self.horizontal_scale,
            ((offset.1 as f32) * self.vertical_scale).round() as isize,
        );
        let horiz_shift = offset.0;
        let copy_or_shift = |src: &mut [f32], dst: &mut [f32]| {
            if offset.0.abs() == 0.0 {
                dst.copy_from_slice(src);
            } else {
                shift_row_to(
                    src,
                    dst,
                    horiz_shift,
                    BoundaryHandling::Constant(0.0),
                    self.level,
                );
            }
        };

        let width = yiq.dimensions.0;
        let height = yiq.num_rows();

        match offset.1.cmp(&0) {
            cmp::Ordering::Less => {
                let offset = (-offset.1) as usize;
                // Starting from the top, copy (or write a horizontally-shifted copy of) each row upwards.
                for dst_row_idx in 0..height {
                    let (i_dst_part, i_src_part) = yiq.i.split_at_mut((dst_row_idx + 1) * width);
                    let (q_dst_part, q_src_part) = yiq.q.split_at_mut((dst_row_idx + 1) * width);

                    let dst_row_range = dst_row_idx * width..(dst_row_idx + 1) * width;
                    let dst_i = &mut i_dst_part[dst_row_range.clone()];
                    let dst_q = &mut q_dst_part[dst_row_range.clone()];

                    if dst_row_idx >= height.max(offset) - offset {
                        dst_i.fill(0.0);
                        dst_q.fill(0.0);
                    } else {
                        let src_row_range = (offset - 1) * width..offset * width;
                        let src_i = &mut i_src_part[src_row_range.clone()];
                        let src_q = &mut q_src_part[src_row_range.clone()];

                        copy_or_shift(src_i, dst_i);
                        copy_or_shift(src_q, dst_q);
                    }
                }
            }
            cmp::Ordering::Equal => {
                // Only a horizontal shift is necessary. We can do this in-place easily.
                ZipChunks::new([yiq.i, yiq.q], width).par_for_each(|_, [i, q]| {
                    shift_row(i, horiz_shift, BoundaryHandling::Constant(0.0), self.level);
                    shift_row(q, horiz_shift, BoundaryHandling::Constant(0.0), self.level);
                });
            }
            cmp::Ordering::Greater => {
                // Some finagling is required to shift vertically. This branch shifts the chroma planes downwards.
                let offset = offset.1 as usize;
                // Starting from the bottom, copy (or write a horizontally-shifted copy of) each row downwards.
                for dst_row_idx in (0..height).rev() {
                    let (i_src_part, i_dst_part) = yiq.i.split_at_mut(dst_row_idx * width);
                    let (q_src_part, q_dst_part) = yiq.q.split_at_mut(dst_row_idx * width);

                    let dst_row_range = 0..width;
                    let dst_i = &mut i_dst_part[dst_row_range.clone()];
                    let dst_q = &mut q_dst_part[dst_row_range.clone()];

                    if dst_row_idx < offset {
                        dst_i.fill(0.0);
                        dst_q.fill(0.0);
                    } else {
                        let src_row_idx = dst_row_idx - offset;
                        let src_row_range = src_row_idx * width..(src_row_idx + 1) * width;
                        let src_i = &mut i_src_part[src_row_range.clone()];
                        let src_q = &mut q_src_part[src_row_range.clone()];

                        copy_or_shift(src_i, dst_i);
                        copy_or_shift(src_q, dst_q);
                    }
                }
            }
        };
    }

    /// Emulate VHS waviness / horizontal shift noise.
    fn vhs_edge_wave(&self, yiq: &mut YiqView, settings: &VHSEdgeWaveSettings) {
        let width = yiq.dimensions.0;
        let height = yiq.num_rows();

        let mut rng = SplitMix64::new(self.seed).mix(noise_seeds::EDGE_WAVE);
        let noise_seed = rng.random::<i32>();
        let offset = rng.random::<f32>() * yiq.num_rows() as f32;
        let noise_dest = &mut yiq.scratch[..height];

        let noise = Fbm {
            seed: noise_seed,
            octaves: settings.detail.clamp(1, 5) as usize,
            gain: FRAC_1_SQRT_2,
            lacunarity: 2.0,
            frequency: settings.frequency / self.vertical_scale,
        };
        sample_noise_2d::<Simplex2d, _>(
            self.level,
            &noise,
            [offset, self.frame_num as f32 * settings.speed],
            [height, 1],
            noise_dest,
        );

        for plane in [&mut yiq.y, &mut yiq.i, &mut yiq.q] {
            ZipChunks::new([plane], width).par_for_each(|index, [row]| {
                let shift =
                    (noise_dest[index] / 0.022) * settings.intensity * 0.5 * self.horizontal_scale;
                shift_row(row, shift, BoundaryHandling::Extend, self.level);
            })
        }
    }

    /// Drop out the chrominance signal from random lines.
    fn chroma_loss(&self, yiq: &mut YiqView, intensity: f32) {
        let width = yiq.dimensions.0;
        let height = yiq.num_rows();

        let mut rng = SplitMix64::new(self.seed)
            .mix(noise_seeds::CHROMA_LOSS)
            .mix(self.frame_num as u64);

        // We blank out each row with a probability of `intensity` (0 to 1). Instead of going over each row and checking
        // whether to blank out the chroma, use a geometric distribution to simulate that process and tell us which rows
        // to blank.
        let dist = geometric_lambda(intensity as f64);

        let mut row_idx = 0usize;
        loop {
            row_idx += rng.random_geometric(dist);
            if row_idx >= height {
                break;
            }

            let row_range = row_idx * width..(row_idx + 1) * width;
            yiq.i[row_range.clone()].fill(0.0);
            yiq.q[row_range.clone()].fill(0.0);
            row_idx += 1;
        }
    }

    /// Vertically blend each chroma scanline with the one above it, as VHS does.
    fn chroma_vert_blend(&self, yiq: &mut YiqView) {
        if yiq.dimensions.1 < 2 {
            return;
        }
        let width = yiq.dimensions.0;
        let delay = &mut yiq.scratch[..width * 2];
        delay.fill(0.0);
        let (delay_i, delay_q) = delay.split_at_mut(width);

        thread_pool::join(
            || {
                yiq.i.chunks_exact_mut(width).for_each(|row| {
                    row.iter_mut().enumerate().for_each(|(index, i)| {
                        let c_i = *i;
                        *i = (delay_i[index] + c_i) * 0.5;
                        delay_i[index] = c_i;
                    });
                });
            },
            || {
                yiq.q.chunks_exact_mut(width).for_each(|row| {
                    row.iter_mut().enumerate().for_each(|(index, q)| {
                        let c_q = *q;
                        *q = (delay_q[index] + c_q) * 0.5;
                        delay_q[index] = c_q;
                    });
                });
            },
        );
    }
}

impl NtscEffect {
    fn apply_effect_to_yiq_field(
        &self,
        ctx: &Context,
        yiq: &mut YiqView,
        frame_num: usize,
        scale_factor: [f32; 2],
    ) {
        let width = yiq.dimensions.0;

        let ctx = EffectCtx::new(ctx, self, yiq.dimensions.1, frame_num, scale_factor);

        ctx.luma_filter(yiq, self.input_luma_filter);

        match self.chroma_lowpass_in {
            ChromaLowpass::Full => {
                ctx.composite_chroma_lowpass(yiq, self.filter_type);
            }
            ChromaLowpass::Light => {
                ctx.composite_chroma_lowpass_lite(yiq, self.filter_type);
            }
            ChromaLowpass::None => {}
        };

        ctx.chroma_into_luma(
            yiq,
            self.video_scanline_phase_shift,
            self.video_scanline_phase_shift_offset,
        );

        if self.composite_sharpening != 0.0 {
            let preemphasis_filter = make_lowpass(
                (315000000.0 / 88.0 / 2.0) * ctx.horizontal_scale,
                NTSC_RATE * ctx.horizontal_scale,
            )
            .with_scale(-self.composite_sharpening);
            ctx.filter_plane(yiq.y, width, &preemphasis_filter, InitialCondition::Zero, 0);
        }

        if self.composite_noise.enabled {
            ctx.composite_noise(yiq, &self.composite_noise.settings);
        }

        if self.snow_intensity > 0.0 && ctx.horizontal_scale > 0.0 {
            ctx.snow(yiq, self.snow_intensity * 0.01, self.snow_anisotropy);
        }

        if self.head_switching.enabled {
            let HeadSwitchingSettings {
                height,
                offset,
                horiz_shift,
                mid_line,
            } = &self.head_switching.settings;
            ctx.head_switching(
                yiq,
                (*height).try_into().unwrap_or_default(),
                (*offset).try_into().unwrap_or_default(),
                *horiz_shift,
                mid_line.as_option(),
            );
        }

        if self.tracking_noise.enabled {
            let TrackingNoiseSettings {
                height,
                wave_intensity,
                snow_intensity,
                snow_anisotropy,
                noise_intensity,
            } = self.tracking_noise.settings;

            ctx.tracking_noise(
                yiq,
                height.try_into().unwrap_or_default(),
                wave_intensity,
                snow_intensity,
                snow_anisotropy,
                noise_intensity,
            );
        }

        ctx.luma_into_chroma(
            yiq,
            self.chroma_demodulation,
            self.video_scanline_phase_shift,
            self.video_scanline_phase_shift_offset,
        );

        if self.luma_smear > 0.0 {
            ctx.luma_smear(yiq, self.luma_smear);
        }

        if self.ringing.enabled {
            let notch_filter = make_notch_filter(
                (self.ringing.settings.frequency / ctx.horizontal_scale).clamp(0.0, 1.0),
                self.ringing.settings.power,
            )
            .with_scale(self.ringing.settings.intensity);
            ctx.filter_plane(
                yiq.y,
                width,
                &notch_filter,
                InitialCondition::FirstSample,
                1,
            );
        }

        if self.luma_noise.enabled {
            ctx.plane_noise(
                yiq.y,
                yiq.dimensions.0,
                &self.luma_noise.settings,
                noise_seeds::VIDEO_LUMA,
            );
        }

        if self.chroma_noise.enabled {
            ctx.plane_noise(
                yiq.i,
                yiq.dimensions.0,
                &self.chroma_noise.settings,
                noise_seeds::VIDEO_CHROMA_I,
            );
            ctx.plane_noise(
                yiq.q,
                yiq.dimensions.0,
                &self.chroma_noise.settings,
                noise_seeds::VIDEO_CHROMA_Q,
            );
        }

        if self.chroma_phase_error > 0.0 {
            ctx.chroma_phase_error(yiq, self.chroma_phase_error);
        }

        if self.chroma_phase_noise_intensity > 0.0 {
            ctx.chroma_phase_noise(yiq, self.chroma_phase_noise_intensity);
        }

        if self.chroma_delay_horizontal != 0.0 || self.chroma_delay_vertical != 0 {
            ctx.chroma_delay(
                yiq,
                (
                    self.chroma_delay_horizontal,
                    self.chroma_delay_vertical as isize,
                ),
            );
        }

        if self.vhs_settings.enabled {
            if self.vhs_settings.settings.edge_wave.enabled
                && self.vhs_settings.settings.edge_wave.settings.intensity > 0.0
            {
                ctx.vhs_edge_wave(yiq, &self.vhs_settings.settings.edge_wave.settings);
            }

            if let Some(VHSTapeParams {
                luma_cut,
                chroma_cut,
                chroma_delay,
            }) = self.vhs_settings.settings.tape_speed.filter_params()
            {
                let chroma_delay = (chroma_delay as f32 * ctx.horizontal_scale).round() as usize;
                // TODO: add an option to control whether there should be a line on the left from the filter starting
                // at 0. it's present in both the original C++ code and Python port but probably not an actual VHS
                // TODO: use a better filter! this effect's output looks way more smear-y than real VHS
                let luma_filter = make_lowpass_for_type(
                    luma_cut,
                    NTSC_RATE * ctx.horizontal_scale,
                    self.filter_type,
                );
                let chroma_filter = make_lowpass_for_type(
                    chroma_cut,
                    NTSC_RATE * ctx.horizontal_scale,
                    self.filter_type,
                );

                thread_pool::join(
                    || ctx.filter_plane(yiq.y, width, &luma_filter, InitialCondition::Zero, 0),
                    || {
                        thread_pool::join(
                            || {
                                ctx.filter_plane(
                                    yiq.i,
                                    width,
                                    &chroma_filter,
                                    InitialCondition::Zero,
                                    chroma_delay,
                                )
                            },
                            || {
                                ctx.filter_plane(
                                    yiq.q,
                                    width,
                                    &chroma_filter,
                                    InitialCondition::Zero,
                                    chroma_delay,
                                )
                            },
                        )
                    },
                );

                let luma_filter_single =
                    make_lowpass(luma_cut, NTSC_RATE * ctx.horizontal_scale).with_scale(-1.6);
                ctx.filter_plane(yiq.y, width, &luma_filter_single, InitialCondition::Zero, 0);
            }

            if self.vhs_settings.settings.chroma_loss > 0.0 {
                ctx.chroma_loss(yiq, self.vhs_settings.settings.chroma_loss);
            }

            if self.vhs_settings.settings.sharpen.enabled
                && let Some(VHSTapeParams { luma_cut, .. }) =
                    self.vhs_settings.settings.tape_speed.filter_params()
            {
                let sharpen = &self.vhs_settings.settings.sharpen.settings;
                let frequency_extra_multiplier = match self.filter_type {
                    FilterType::ConstantK => 4.0,
                    FilterType::Butterworth => 1.0,
                };
                let luma_sharpen_filter = make_lowpass_for_type(
                    luma_cut * frequency_extra_multiplier * sharpen.frequency,
                    NTSC_RATE * ctx.horizontal_scale,
                    self.filter_type,
                )
                .with_scale(-sharpen.intensity * 2.0 * sharpen.frequency);
                // The composite-video-simulator code sharpens the chroma plane, but ntscqt and this effect do not.
                // I'm not sure if I'm implementing it wrong, but chroma sharpening looks awful.
                /*let chroma_sharpen_filter = make_lowpass_for_type(
                    chroma_cut * frequency_extra_multiplier * sharpen.frequency,
                    NTSC_RATE * info.horizontal_scale,
                    self.filter_type,
                ).with_scale(-sharpen.intensity * 0.85 * sharpen.frequency);*/
                ctx.filter_plane(
                    yiq.y,
                    width,
                    &luma_sharpen_filter,
                    InitialCondition::Zero,
                    0,
                );
                /*ctx.filter_plane(
                    yiq.i,
                    width,
                    &chroma_sharpen_filter,
                    InitialCondition::Zero,
                    0,
                );
                ctx.filter_plane(
                    yiq.q,
                    width,
                    &chroma_sharpen_filter,
                    InitialCondition::Zero,
                    0,
                );*/
            }
        }

        if self.chroma_vert_blend {
            ctx.chroma_vert_blend(yiq);
        }

        match self.chroma_lowpass_out {
            ChromaLowpass::Full => {
                ctx.composite_chroma_lowpass(yiq, self.filter_type);
            }
            ChromaLowpass::Light => {
                ctx.composite_chroma_lowpass_lite(yiq, self.filter_type);
            }
            ChromaLowpass::None => {}
        };
    }

    fn apply_effect_to_all_fields(
        &self,
        ctx: &Context,
        yiq: &mut YiqView,
        frame_num: usize,
        scale_factor: [f32; 2],
    ) {
        match yiq.field {
            YiqField::Upper | YiqField::Lower | YiqField::Both => {
                self.apply_effect_to_yiq_field(ctx, yiq, frame_num, scale_factor);
            }
            YiqField::InterleavedUpper | YiqField::InterleavedLower => {
                // "Interleaved" basically means we apply the effect to one set of fields, then apply it again to the
                // other set of fields with an increased frame number.
                let (mut yiq_upper, mut yiq_lower, frame_num_upper, frame_num_lower) =
                    // With the "interleaved" field option, the image is blitted into the YIQ buffer with the even/odd
                    // fields in the top half and the odd/even fields in the bottom half.
                    match yiq.field {
                        YiqField::InterleavedUpper => {
                            let num_upper_rows = YiqField::Upper.num_actual_image_rows(yiq.dimensions.1);
                            let (upper, lower) = yiq.split_at_row(num_upper_rows);
                            (upper, lower, frame_num * 2, frame_num * 2 + 1)
                        }
                        YiqField::InterleavedLower => {
                            let num_lower_rows = YiqField::Lower.num_actual_image_rows(yiq.dimensions.1);
                            let (lower, upper) = yiq.split_at_row(num_lower_rows);
                            (upper, lower, frame_num * 2 + 1, frame_num * 2)
                        }
                        _ => unreachable!(),
                    };

                if let Some(yiq_upper) = yiq_upper.as_mut() {
                    yiq_upper.field = YiqField::Upper;
                    self.apply_effect_to_yiq_field(ctx, yiq_upper, frame_num_upper, scale_factor);
                }

                if let Some(yiq_lower) = yiq_lower.as_mut() {
                    yiq_lower.field = YiqField::Lower;
                    self.apply_effect_to_yiq_field(ctx, yiq_lower, frame_num_lower, scale_factor);
                }
            }
        }
    }

    /// Apply the effect to YIQ image data.
    pub fn apply_effect_to_yiq(
        &self,
        ctx: &Context,
        yiq: &mut YiqView,
        frame_num: usize,
        scale_factor: [f32; 2],
    ) {
        ctx.thread_pool
            .install(|| self.apply_effect_to_all_fields(ctx, yiq, frame_num, scale_factor));
    }

    /// Apply the effect to a buffer which contains pixels in the given format.
    /// Convenience function meant mainly for tests--see the yiq_fielding module for doing things more efficiently, like
    /// reusing the output buffer.
    pub fn apply_effect_to_buffer<S: PixelFormat, T: Normalize>(
        &self,
        ctx: &Context,
        dimensions: (usize, usize),
        input_frame: &mut [T],
        frame_num: usize,
        scale_factor: [f32; 2],
    ) {
        let field = self.use_field.to_yiq_field(frame_num);
        let row_bytes = dimensions.0 * pixel_bytes_for::<S, T>();
        let mut yiq = YiqOwned::from_strided_buffer::<S, T>(
            ctx,
            input_frame,
            row_bytes,
            dimensions.0,
            dimensions.1,
            field,
        );
        let mut view = YiqView::from(&mut yiq);
        self.apply_effect_to_yiq(ctx, &mut view, frame_num, scale_factor);
        view.write_to_strided_buffer::<S, T, _>(
            ctx,
            input_frame,
            BlitInfo::from_full_frame(dimensions.0, dimensions.1, row_bytes),
            crate::yiq_fielding::DeinterlaceMode::Bob,
            (),
        );
    }
}
