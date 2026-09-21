use alloc::{borrow::Cow, boxed::Box, vec};

use crate::{
    settings::{JsonValue, SettingsBlock, SortedMap},
    yiq_fielding::YiqField,
};
use num_enum::{IntoPrimitive, TryFromPrimitive};

use super::{
    GetAndExpect, MenuItem, ParseSettingsError, SettingDescriptor, SettingKind, Settings,
    SettingsEnum, SettingsList, parse_json,
};

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, TryFromPrimitive, IntoPrimitive)]
pub enum UseField {
    Alternating = 0,
    Upper,
    Lower,
    Both,
    InterleavedUpper,
    InterleavedLower,
}
impl SettingsEnum for UseField {}

impl UseField {
    pub fn to_yiq_field(&self, frame_num: usize) -> YiqField {
        match self {
            UseField::Alternating => {
                if frame_num & 1 == 0 {
                    YiqField::Lower
                } else {
                    YiqField::Upper
                }
            }
            UseField::Upper => YiqField::Upper,
            UseField::Lower => YiqField::Lower,
            UseField::Both => YiqField::Both,
            UseField::InterleavedUpper => YiqField::InterleavedUpper,
            UseField::InterleavedLower => YiqField::InterleavedLower,
        }
    }

    pub fn interlaced_output_allowed(&self) -> bool {
        matches!(
            self,
            UseField::InterleavedUpper | UseField::InterleavedLower
        )
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, TryFromPrimitive, IntoPrimitive)]
pub enum FilterType {
    ConstantK = 0,
    Butterworth,
}
impl SettingsEnum for FilterType {}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, TryFromPrimitive, IntoPrimitive)]
pub enum LumaLowpass {
    None,
    Box,
    Notch,
}
impl SettingsEnum for LumaLowpass {}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, TryFromPrimitive, IntoPrimitive)]
pub enum VHSTapeSpeed {
    NONE,
    SP,
    LP,
    EP,
}
impl SettingsEnum for VHSTapeSpeed {}

pub(crate) struct VHSTapeParams {
    pub luma_cut: f32,
    pub chroma_cut: f32,
    pub chroma_delay: usize,
}

impl VHSTapeSpeed {
    pub(crate) fn filter_params(&self) -> Option<VHSTapeParams> {
        match self {
            Self::NONE => None,
            Self::SP => Some(VHSTapeParams {
                luma_cut: 2400000.0,
                chroma_cut: 320000.0,
                chroma_delay: 4,
            }),
            Self::LP => Some(VHSTapeParams {
                luma_cut: 1900000.0,
                chroma_cut: 300000.0,
                chroma_delay: 5,
            }),
            Self::EP => Some(VHSTapeParams {
                luma_cut: 1400000.0,
                chroma_cut: 280000.0,
                chroma_delay: 6,
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct VHSSharpenSettings {
    pub intensity: f32,
    pub frequency: f32,
}

impl Default for VHSSharpenSettings {
    fn default() -> Self {
        Self {
            intensity: 0.25,
            frequency: 1.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct VHSEdgeWaveSettings {
    pub intensity: f32,
    pub speed: f32,
    pub frequency: f32,
    pub detail: i32,
}

impl Default for VHSEdgeWaveSettings {
    fn default() -> Self {
        Self {
            intensity: 0.5,
            speed: 4.0,
            frequency: 0.05,
            detail: 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct VHSSettings {
    pub tape_speed: VHSTapeSpeed,
    pub chroma_loss: f32,
    pub sharpen: SettingsBlock<VHSSharpenSettings>,
    pub edge_wave: SettingsBlock<VHSEdgeWaveSettings>,
}

impl Default for VHSSettings {
    fn default() -> Self {
        Self {
            tape_speed: VHSTapeSpeed::LP,
            chroma_loss: 0.000025,
            sharpen: Default::default(),
            edge_wave: Default::default(),
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, TryFromPrimitive, IntoPrimitive)]
pub enum ChromaLowpass {
    None,
    Light,
    Full,
}
impl SettingsEnum for ChromaLowpass {}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, TryFromPrimitive, IntoPrimitive)]
pub enum ChromaDemodulationFilter {
    Box,
    Notch,
    OneLineComb,
    TwoLineComb,
    TwoD,
    TwoDAdaptive,
}
impl SettingsEnum for ChromaDemodulationFilter {}

#[derive(Debug, Clone, PartialEq)]
pub struct HeadSwitchingMidLineSettings {
    pub position: f32,
    pub jitter: f32,
}

impl Default for HeadSwitchingMidLineSettings {
    fn default() -> Self {
        Self {
            position: 0.95,
            jitter: 0.03,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct HeadSwitchingSettings {
    pub height: i32,
    pub offset: i32,
    pub horiz_shift: f32,
    pub mid_line: SettingsBlock<HeadSwitchingMidLineSettings>,
}

impl Default for HeadSwitchingSettings {
    fn default() -> Self {
        Self {
            height: 8,
            offset: 3,
            horiz_shift: 72.0,
            mid_line: Default::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TrackingNoiseSettings {
    pub height: i32,
    pub wave_intensity: f32,
    pub snow_intensity: f32,
    pub snow_anisotropy: f32,
    pub noise_intensity: f32,
}

impl Default for TrackingNoiseSettings {
    fn default() -> Self {
        Self {
            height: 12,
            wave_intensity: 15.0,
            snow_intensity: 0.025,
            snow_anisotropy: 0.25,
            noise_intensity: 0.25,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RingingSettings {
    pub frequency: f32,
    pub power: f32,
    pub intensity: f32,
}

impl Default for RingingSettings {
    fn default() -> Self {
        Self {
            frequency: 0.45,
            power: 4.0,
            intensity: 4.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FbmNoiseSettings {
    pub frequency: f32,
    pub intensity: f32,
    pub detail: i32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScaleSettings {
    pub horizontal_scale: f32,
    pub vertical_scale: f32,
    pub scale_with_video_size: bool,
}

impl Default for ScaleSettings {
    fn default() -> Self {
        Self {
            horizontal_scale: 1.0,
            vertical_scale: 1.0,
            scale_with_video_size: false,
        }
    }
}

#[rustfmt::skip]
pub mod setting_id {
    use crate::{setting_id, settings::SettingID};
    use super::NtscEffect;
    type NtscSettingID = SettingID<NtscEffect>;

    // All of the setting IDs. I try to keep deleted ones commented out here for context.
    pub const CHROMA_LOWPASS_IN: NtscSettingID = setting_id!(0, "chroma_lowpass_in", chroma_lowpass_in);
    pub const COMPOSITE_SHARPENING: NtscSettingID = setting_id!(1, "composite_preemphasis", composite_sharpening);
    //pub const VIDEO_SCANLINE_PHASE_SHIFT: NtscSettingID = setting_id!(2, "video_scanline_phase_shift", video_scanline_phase_shift);
    //pub const VIDEO_SCANLINE_PHASE_SHIFT_OFFSET: NtscSettingID = setting_id!(3, "video_scanline_phase_shift_offset", video_scanline_phase_shift_offset);
    pub const COMPOSITE_NOISE_INTENSITY: NtscSettingID = setting_id!(4, "composite_noise_intensity", composite_noise.settings.intensity);
    pub const CHROMA_NOISE_INTENSITY: NtscSettingID = setting_id!(5, "chroma_noise_intensity", chroma_noise.settings.intensity);
    pub const SNOW_INTENSITY: NtscSettingID = setting_id!(6, "snow_intensity", snow_intensity);
    pub const CHROMA_PHASE_NOISE_INTENSITY: NtscSettingID = setting_id!(7, "chroma_phase_noise_intensity", chroma_phase_noise_intensity);
    pub const CHROMA_DELAY_HORIZONTAL: NtscSettingID = setting_id!(8, "chroma_delay_horizontal", chroma_delay_horizontal);
    pub const CHROMA_DELAY_VERTICAL: NtscSettingID = setting_id!(9, "chroma_delay_vertical", chroma_delay_vertical);
    pub const CHROMA_LOWPASS_OUT: NtscSettingID = setting_id!(10, "chroma_lowpass_out", chroma_lowpass_out);
    pub const HEAD_SWITCHING: NtscSettingID = setting_id!(11, "head_switching", head_switching.enabled);
    pub const HEAD_SWITCHING_HEIGHT: NtscSettingID = setting_id!(12, "head_switching_height", head_switching.settings.height);
    pub const HEAD_SWITCHING_OFFSET: NtscSettingID = setting_id!(13, "head_switching_offset", head_switching.settings.offset);
    pub const HEAD_SWITCHING_HORIZONTAL_SHIFT: NtscSettingID = setting_id!(14, "head_switching_horizontal_shift", head_switching.settings.horiz_shift);
    pub const TRACKING_NOISE: NtscSettingID = setting_id!(15, "tracking_noise", tracking_noise.enabled);
    pub const TRACKING_NOISE_HEIGHT: NtscSettingID = setting_id!(16, "tracking_noise_height", tracking_noise.settings.height);
    pub const TRACKING_NOISE_WAVE_INTENSITY: NtscSettingID = setting_id!(17, "tracking_noise_wave_intensity", tracking_noise.settings.wave_intensity);
    pub const TRACKING_NOISE_SNOW_INTENSITY: NtscSettingID = setting_id!(18, "tracking_noise_snow_intensity", tracking_noise.settings.snow_intensity);
    pub const RINGING: NtscSettingID = setting_id!(19, "ringing", ringing.enabled);
    pub const RINGING_FREQUENCY: NtscSettingID = setting_id!(20, "ringing_frequency", ringing.settings.frequency);
    pub const RINGING_POWER: NtscSettingID = setting_id!(21, "ringing_power", ringing.settings.power);
    pub const RINGING_SCALE: NtscSettingID = setting_id!(22, "ringing_scale", ringing.settings.intensity);
    pub const VHS_SETTINGS: NtscSettingID = setting_id!(23, "vhs_settings", vhs_settings.enabled);
    pub const VHS_TAPE_SPEED: NtscSettingID = setting_id!(24, "vhs_tape_speed", vhs_settings.settings.tape_speed);
    pub const CHROMA_VERT_BLEND: NtscSettingID = setting_id!(25, "vhs_chroma_vert_blend", chroma_vert_blend);
    pub const VHS_CHROMA_LOSS: NtscSettingID = setting_id!(26, "vhs_chroma_loss", vhs_settings.settings.chroma_loss);
    pub const VHS_SHARPEN_INTENSITY: NtscSettingID = setting_id!(27, "vhs_sharpen", vhs_settings.settings.sharpen.settings.intensity);
    pub const VHS_EDGE_WAVE_INTENSITY: NtscSettingID = setting_id!(28, "vhs_edge_wave", vhs_settings.settings.edge_wave.settings.intensity);
    pub const VHS_EDGE_WAVE_SPEED: NtscSettingID = setting_id!(29, "vhs_edge_wave_speed", vhs_settings.settings.edge_wave.settings.speed);
    pub const USE_FIELD: NtscSettingID = setting_id!(30, "use_field", use_field);
    pub const TRACKING_NOISE_NOISE_INTENSITY: NtscSettingID = setting_id!(31, "tracking_noise_noise_intensity", tracking_noise.settings.noise_intensity);
    pub const HORIZONTAL_SCALE: NtscSettingID = setting_id!(32, "bandwidth_scale", scale.settings.horizontal_scale);
    pub const CHROMA_DEMODULATION: NtscSettingID = setting_id!(33, "chroma_demodulation", chroma_demodulation);
    pub const SNOW_ANISOTROPY: NtscSettingID = setting_id!(34, "snow_anisotropy", snow_anisotropy);
    pub const TRACKING_NOISE_SNOW_ANISOTROPY: NtscSettingID = setting_id!(35, "tracking_noise_snow_anisotropy", tracking_noise.settings.snow_anisotropy);
    pub const RANDOM_SEED: NtscSettingID = setting_id!(36, "random_seed", random_seed);
    pub const CHROMA_PHASE_ERROR: NtscSettingID = setting_id!(37, "chroma_phase_error", chroma_phase_error);
    pub const INPUT_LUMA_FILTER: NtscSettingID = setting_id!(38, "input_luma_filter", input_luma_filter);
    pub const VHS_EDGE_WAVE_ENABLED: NtscSettingID = setting_id!(39, "vhs_edge_wave_enabled", vhs_settings.settings.edge_wave.enabled);
    pub const VHS_EDGE_WAVE_FREQUENCY: NtscSettingID = setting_id!(40, "vhs_edge_wave_frequency", vhs_settings.settings.edge_wave.settings.frequency);
    pub const VHS_EDGE_WAVE_DETAIL: NtscSettingID = setting_id!(41, "vhs_edge_wave_detail", vhs_settings.settings.edge_wave.settings.detail);
    pub const CHROMA_NOISE: NtscSettingID = setting_id!(42, "chroma_noise", chroma_noise.enabled);
    pub const CHROMA_NOISE_FREQUENCY: NtscSettingID = setting_id!(43, "chroma_noise_frequency", chroma_noise.settings.frequency);
    pub const CHROMA_NOISE_DETAIL: NtscSettingID = setting_id!(44, "chroma_noise_detail", chroma_noise.settings.detail);
    pub const LUMA_SMEAR: NtscSettingID = setting_id!(45, "luma_smear", luma_smear);
    pub const FILTER_TYPE: NtscSettingID = setting_id!(46, "filter_type", filter_type);
    pub const VHS_SHARPEN_ENABLED: NtscSettingID = setting_id!(47, "vhs_sharpen_enabled", vhs_settings.settings.sharpen.enabled);
    pub const VHS_SHARPEN_FREQUENCY: NtscSettingID = setting_id!(48, "vhs_sharpen_frequency", vhs_settings.settings.sharpen.settings.frequency);
    pub const HEAD_SWITCHING_START_MID_LINE: NtscSettingID = setting_id!(49, "head_switching_start_mid_line", head_switching.settings.mid_line.enabled);
    pub const HEAD_SWITCHING_MID_LINE_POSITION: NtscSettingID = setting_id!(50, "head_switching_mid_line_position", head_switching.settings.mid_line.settings.position);
    pub const HEAD_SWITCHING_MID_LINE_JITTER: NtscSettingID = setting_id!(51, "head_switching_mid_line_jitter", head_switching.settings.mid_line.settings.jitter);
    pub const COMPOSITE_NOISE: NtscSettingID = setting_id!(52, "composite_noise", composite_noise.enabled);
    pub const COMPOSITE_NOISE_FREQUENCY: NtscSettingID = setting_id!(53, "composite_noise_frequency", composite_noise.settings.frequency);
    pub const COMPOSITE_NOISE_DETAIL: NtscSettingID = setting_id!(54, "composite_noise_detail", composite_noise.settings.detail);
    pub const LUMA_NOISE: NtscSettingID = setting_id!(55, "luma_noise", luma_noise.enabled);
    pub const LUMA_NOISE_FREQUENCY: NtscSettingID = setting_id!(56, "luma_noise_frequency", luma_noise.settings.frequency);
    pub const LUMA_NOISE_INTENSITY: NtscSettingID = setting_id!(57, "luma_noise_intensity", luma_noise.settings.intensity);
    pub const LUMA_NOISE_DETAIL: NtscSettingID = setting_id!(58, "luma_noise_detail", luma_noise.settings.detail);
    pub const VERTICAL_SCALE: NtscSettingID = setting_id!(59, "vertical_scale", scale.settings.vertical_scale);
    pub const SCALE_WITH_VIDEO_SIZE: NtscSettingID = setting_id!(60, "scale_with_video_size", scale.settings.scale_with_video_size);
    pub const SCALE_SETTINGS: NtscSettingID = setting_id!(61, "scale_settings", scale.enabled);
}

#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct NtscEffect {
    pub random_seed: i32,
    pub use_field: UseField,
    pub filter_type: FilterType,
    pub input_luma_filter: LumaLowpass,
    pub chroma_lowpass_in: ChromaLowpass,
    pub chroma_demodulation: ChromaDemodulationFilter,
    pub luma_smear: f32,
    pub composite_sharpening: f32,
    pub head_switching: SettingsBlock<HeadSwitchingSettings>,
    pub tracking_noise: SettingsBlock<TrackingNoiseSettings>,
    pub composite_noise: SettingsBlock<FbmNoiseSettings>,
    pub ringing: SettingsBlock<RingingSettings>,
    pub luma_noise: SettingsBlock<FbmNoiseSettings>,
    pub chroma_noise: SettingsBlock<FbmNoiseSettings>,
    pub snow_intensity: f32,
    pub snow_anisotropy: f32,
    pub chroma_phase_noise_intensity: f32,
    pub chroma_phase_error: f32,
    pub chroma_delay_horizontal: f32,
    pub chroma_delay_vertical: i32,
    pub vhs_settings: SettingsBlock<VHSSettings>,
    pub chroma_vert_blend: bool,
    pub chroma_lowpass_out: ChromaLowpass,
    pub scale: SettingsBlock<ScaleSettings>,
}

impl Default for NtscEffect {
    fn default() -> Self {
        Self {
            random_seed: 0,
            use_field: UseField::InterleavedUpper,
            filter_type: FilterType::Butterworth,
            input_luma_filter: LumaLowpass::Notch,
            chroma_lowpass_in: ChromaLowpass::Full,
            chroma_demodulation: ChromaDemodulationFilter::Notch,
            luma_smear: 0.5,
            chroma_lowpass_out: ChromaLowpass::Full,
            composite_sharpening: 1.0,
            head_switching: Default::default(),
            tracking_noise: Default::default(),
            ringing: Default::default(),
            snow_intensity: 0.00025,
            snow_anisotropy: 0.5,
            composite_noise: SettingsBlock::enabled(FbmNoiseSettings {
                frequency: 0.5,
                intensity: 0.05,
                detail: 1,
            }),
            luma_noise: SettingsBlock::enabled(FbmNoiseSettings {
                frequency: 0.5,
                intensity: 0.01,
                detail: 1,
            }),
            chroma_noise: SettingsBlock::enabled(FbmNoiseSettings {
                frequency: 0.05,
                intensity: 0.1,
                detail: 2,
            }),
            chroma_phase_noise_intensity: 0.001,
            chroma_phase_error: 0.0,
            chroma_delay_horizontal: 0.0,
            chroma_delay_vertical: 0,
            vhs_settings: Default::default(),
            chroma_vert_blend: false,
            scale: Default::default(),
        }
    }
}

impl Settings for NtscEffect {
    fn setting_descriptors() -> Box<[SettingDescriptor<Self>]> {
        vec![
            SettingDescriptor {
                label: "Random seed",
                description: None,
                kind: SettingKind::IntRange {
                    range: i32::MIN..=i32::MAX,
                },
                id: setting_id::RANDOM_SEED,
            },
            SettingDescriptor {
                label: "Use field",
                description: Some(
                    "Choose which rows (\"fields\" in NTSC parlance) of the source image will be \
                     used.",
                ),
                kind: SettingKind::Enumeration {
                    options: vec![
                        MenuItem {
                            label: "Alternating",
                            description: Some(
                                "Skip every other row, alternating between skipping even and odd \
                                 rows.",
                            ),
                            index: UseField::Alternating as u32,
                        },
                        MenuItem {
                            label: "Upper only",
                            description: Some("Skip every lower row, keeping the upper ones."),
                            index: UseField::Upper as u32,
                        },
                        MenuItem {
                            label: "Lower only",
                            description: Some("Skip every upper row, keeping the lower ones."),
                            index: UseField::Lower as u32,
                        },
                        MenuItem {
                            label: "Interleaved (upper first)",
                            description: Some(
                                "Treat the video as interlaced, with the upper field as the \
                                 earlier frame.",
                            ),
                            index: UseField::InterleavedUpper as u32,
                        },
                        MenuItem {
                            label: "Interleaved (lower first)",
                            description: Some(
                                "Treat the video as interlaced, with the lower field as the \
                                 earlier frame.",
                            ),
                            index: UseField::InterleavedLower as u32,
                        },
                        MenuItem {
                            label: "Both",
                            description: Some("Use all rows; don't skip any."),
                            index: UseField::Both as u32,
                        },
                    ],
                },
                id: setting_id::USE_FIELD,
            },
            SettingDescriptor {
                label: "Lowpass filter type",
                description: Some("The low-pass filter to use throughout the effect."),
                kind: SettingKind::Enumeration {
                    options: vec![
                        MenuItem {
                            label: "Constant K (blurry)",
                            description: Some(
                                "Simple constant-k filter. Produces longer, blurry results.",
                            ),
                            index: FilterType::ConstantK as u32,
                        },
                        MenuItem {
                            label: "Butterworth (sharper)",
                            description: Some(
                                "Filter with a sharper falloff. Produces sharpened, less blurry \
                                 results.",
                            ),
                            index: FilterType::Butterworth as u32,
                        },
                    ],
                },
                id: setting_id::FILTER_TYPE,
            },
            SettingDescriptor {
                label: "Input luma filter",
                description: Some("Filter the input luminance to decrease rainbow artifacts."),
                kind: SettingKind::Enumeration {
                    options: vec![
                        MenuItem {
                            label: "Notch",
                            description: Some(
                                "Apply a notch filter to the input luminance signal. Sharp, but \
                                 has ringing artifacts.",
                            ),
                            index: LumaLowpass::Notch as u32,
                        },
                        MenuItem {
                            label: "Box",
                            description: Some(
                                "Apply a simple box filter to the input luminance signal.",
                            ),
                            index: LumaLowpass::Box as u32,
                        },
                        MenuItem {
                            label: "None",
                            description: Some(
                                "Do not filter the luminance signal. Adds rainbow artifacts.",
                            ),
                            index: LumaLowpass::None as u32,
                        },
                    ],
                },
                id: setting_id::INPUT_LUMA_FILTER,
            },
            SettingDescriptor {
                label: "Chroma low-pass in",
                description: Some(
                    "Apply a low-pass filter to the input chrominance (color) signal.",
                ),
                kind: SettingKind::Enumeration {
                    options: vec![
                        MenuItem {
                            label: "Full",
                            description: Some("Full-intensity low-pass filter."),
                            index: ChromaLowpass::Full as u32,
                        },
                        MenuItem {
                            label: "Light",
                            description: Some("Less intense low-pass filter."),
                            index: ChromaLowpass::Light as u32,
                        },
                        MenuItem {
                            label: "None",
                            description: Some("No low-pass filter."),
                            index: ChromaLowpass::None as u32,
                        },
                    ],
                },
                id: setting_id::CHROMA_LOWPASS_IN,
            },
            SettingDescriptor {
                label: "Composite signal sharpening",
                description: Some(
                    "Boost high frequencies in the NTSC signal, sharpening the image and \
                     intensifying colors.",
                ),
                kind: SettingKind::FloatRange {
                    range: -1.0..=2.0,
                    logarithmic: false,
                },
                id: setting_id::COMPOSITE_SHARPENING,
            },
            SettingDescriptor {
                label: "Composite signal noise",
                description: Some("Noise applied to the composite NTSC signal."),
                kind: SettingKind::Group {
                    children: vec![
                        SettingDescriptor {
                            label: "Intensity",
                            description: Some("Intensity of the noise."),
                            kind: SettingKind::Percentage { logarithmic: true },
                            id: setting_id::COMPOSITE_NOISE_INTENSITY,
                        },
                        SettingDescriptor {
                            label: "Frequency",
                            description: Some("Base wavelength, in pixels, of the noise."),
                            kind: SettingKind::FloatRange {
                                range: 0.0..=1.0,
                                logarithmic: false,
                            },
                            id: setting_id::COMPOSITE_NOISE_FREQUENCY,
                        },
                        SettingDescriptor {
                            label: "Detail",
                            description: Some("Octaves of noise."),
                            kind: SettingKind::IntRange { range: 1..=5 },
                            id: setting_id::COMPOSITE_NOISE_DETAIL,
                        },
                    ],
                },
                id: setting_id::COMPOSITE_NOISE,
            },
            SettingDescriptor {
                label: "Snow",
                description: Some("Frequency of random speckles in the image."),
                kind: SettingKind::FloatRange {
                    range: 0.0..=100.0,
                    logarithmic: true,
                },
                id: setting_id::SNOW_INTENSITY,
            },
            SettingDescriptor {
                label: "Snow anisotropy",
                description: Some(
                    "Determines whether the speckles are placed truly randomly or concentrated in \
                     certain rows.",
                ),
                kind: SettingKind::Percentage { logarithmic: false },
                id: setting_id::SNOW_ANISOTROPY,
            },
            SettingDescriptor {
                label: "Chroma demodulation filter",
                description: Some(
                    "Filter used to modulate the chrominance (color) data out of the composite \
                     NTSC signal.",
                ),
                kind: SettingKind::Enumeration {
                    options: vec![
                        MenuItem {
                            label: "Box",
                            description: Some("Simple horizontal box blur."),
                            index: ChromaDemodulationFilter::Box as u32,
                        },
                        MenuItem {
                            label: "Notch",
                            description: Some(
                                "Notch filter. Sharper than a box blur, but with ringing \
                                 artifacts.",
                            ),
                            index: ChromaDemodulationFilter::Notch as u32,
                        },
                        MenuItem {
                            label: "2D",
                            description: Some(
                                "2D \"digital-style\" filter. Highest all-around quality.",
                            ),
                            index: ChromaDemodulationFilter::TwoD as u32,
                        },
                        MenuItem {
                            label: "2D adaptive",
                            description: Some(
                                "2D edge-following filter. Avoids \"dot crawl\" artifacts at all \
                                 costs, but results in less detailed output and is likely to cause \
                                 rainbow artifacts.",
                            ),
                            index: ChromaDemodulationFilter::TwoDAdaptive as u32,
                        },
                        MenuItem {
                            label: "1-line comb",
                            description: Some(
                                "Average the current row with the previous one, phase-cancelling \
                                 the chrominance (color) signals.",
                            ),
                            index: ChromaDemodulationFilter::OneLineComb as u32,
                        },
                        MenuItem {
                            label: "2-line comb",
                            description: Some(
                                "Average the current row with the previous and next ones, \
                                 phase-cancelling the chrominance (color) signals.",
                            ),
                            index: ChromaDemodulationFilter::TwoLineComb as u32,
                        },
                    ],
                },
                id: setting_id::CHROMA_DEMODULATION,
            },
            SettingDescriptor {
                label: "Luma smear",
                description: None,
                kind: SettingKind::FloatRange {
                    range: 0.0..=1.0,
                    logarithmic: false,
                },
                id: setting_id::LUMA_SMEAR,
            },
            SettingDescriptor {
                label: "Head switching",
                description: Some(
                    "Emulate VHS head-switching artifacts at the bottom of the image.",
                ),
                kind: SettingKind::Group {
                    children: vec![
                        SettingDescriptor {
                            label: "Height",
                            description: Some("Total height of the head-switching artifact."),
                            kind: SettingKind::IntRange { range: 0..=24 },
                            id: setting_id::HEAD_SWITCHING_HEIGHT,
                        },
                        SettingDescriptor {
                            label: "Offset",
                            description: Some(
                                "How much of the head-switching artifact is off-screen.",
                            ),
                            kind: SettingKind::IntRange { range: 0..=24 },
                            id: setting_id::HEAD_SWITCHING_OFFSET,
                        },
                        SettingDescriptor {
                            label: "Horizontal shift",
                            description: Some(
                                "How much the head-switching artifact shifts rows horizontally.",
                            ),
                            kind: SettingKind::FloatRange {
                                range: -100.0..=100.0,
                                logarithmic: false,
                            },
                            id: setting_id::HEAD_SWITCHING_HORIZONTAL_SHIFT,
                        },
                        SettingDescriptor {
                            label: "Start mid-line",
                            description: Some(
                                "Start the head-switching artifact mid-scanline, with some static \
                                 where it begins.",
                            ),
                            kind: SettingKind::Group {
                                children: vec![
                                    SettingDescriptor {
                                        label: "Position",
                                        description: Some(
                                            "Horizontal position at which the head-switching \
                                             starts.",
                                        ),
                                        kind: SettingKind::Percentage { logarithmic: false },
                                        id: setting_id::HEAD_SWITCHING_MID_LINE_POSITION,
                                    },
                                    SettingDescriptor {
                                        label: "Jitter",
                                        description: Some(
                                            "How much the head-switching artifact \"jitters\" \
                                             horizontally.",
                                        ),
                                        kind: SettingKind::Percentage { logarithmic: true },
                                        id: setting_id::HEAD_SWITCHING_MID_LINE_JITTER,
                                    },
                                ],
                            },
                            id: setting_id::HEAD_SWITCHING_START_MID_LINE,
                        },
                    ],
                },
                id: setting_id::HEAD_SWITCHING,
            },
            SettingDescriptor {
                label: "Tracking noise",
                description: Some("Emulate noise from VHS tracking error."),
                kind: SettingKind::Group {
                    children: vec![
                        SettingDescriptor {
                            label: "Height",
                            description: Some("Total height of the tracking artifacts."),
                            kind: SettingKind::IntRange { range: 0..=120 },
                            id: setting_id::TRACKING_NOISE_HEIGHT,
                        },
                        SettingDescriptor {
                            label: "Wave intensity",
                            description: Some(
                                "How much the affected scanlines \"wave\" back and forth.",
                            ),
                            kind: SettingKind::FloatRange {
                                range: -50.0..=50.0,
                                logarithmic: false,
                            },
                            id: setting_id::TRACKING_NOISE_WAVE_INTENSITY,
                        },
                        SettingDescriptor {
                            label: "Snow intensity",
                            description: Some("Frequency of speckle-type noise in the artifacts."),
                            kind: SettingKind::FloatRange {
                                range: 0.0..=1.0,
                                logarithmic: true,
                            },
                            id: setting_id::TRACKING_NOISE_SNOW_INTENSITY,
                        },
                        SettingDescriptor {
                            label: "Snow anisotropy",
                            description: Some("How much the speckles are clustered by scanline."),
                            kind: SettingKind::Percentage { logarithmic: false },
                            id: setting_id::TRACKING_NOISE_SNOW_ANISOTROPY,
                        },
                        SettingDescriptor {
                            label: "Noise intensity",
                            description: Some("Intensity of non-speckle noise."),
                            kind: SettingKind::Percentage { logarithmic: true },
                            id: setting_id::TRACKING_NOISE_NOISE_INTENSITY,
                        },
                    ],
                },
                id: setting_id::TRACKING_NOISE,
            },
            SettingDescriptor {
                label: "Ringing",
                description: Some("Additional ringing artifacts, simulated with a notch filter."),
                kind: SettingKind::Group {
                    children: vec![
                        SettingDescriptor {
                            label: "Frequency",
                            description: Some(
                                "Frequency/period of the ringing, in \"rings per pixel\".",
                            ),
                            kind: SettingKind::Percentage { logarithmic: false },
                            id: setting_id::RINGING_FREQUENCY,
                        },
                        SettingDescriptor {
                            label: "Power",
                            description: Some(
                                "The power of the notch filter / how far out the ringing extends.",
                            ),
                            kind: SettingKind::FloatRange {
                                range: 1.0..=10.0,
                                logarithmic: false,
                            },
                            id: setting_id::RINGING_POWER,
                        },
                        SettingDescriptor {
                            label: "Scale",
                            description: Some("Intensity of the ringing."),
                            kind: SettingKind::FloatRange {
                                range: 0.0..=10.0,
                                logarithmic: false,
                            },
                            id: setting_id::RINGING_SCALE,
                        },
                    ],
                },
                id: setting_id::RINGING,
            },
            SettingDescriptor {
                label: "Luma noise",
                description: Some(
                    "Noise applied to the luminance signal. Useful for higher-frequency noise \
                     than the \"Composite noise\" setting can provide.",
                ),
                kind: SettingKind::Group {
                    children: vec![
                        SettingDescriptor {
                            label: "Intensity",
                            description: Some("Intensity of the noise."),
                            kind: SettingKind::Percentage { logarithmic: true },
                            id: setting_id::LUMA_NOISE_INTENSITY,
                        },
                        SettingDescriptor {
                            label: "Frequency",
                            description: Some("Base wavelength, in pixels, of the noise."),
                            kind: SettingKind::FloatRange {
                                range: 0.0..=1.0,
                                logarithmic: false,
                            },
                            id: setting_id::LUMA_NOISE_FREQUENCY,
                        },
                        SettingDescriptor {
                            label: "Detail",
                            description: Some("Octaves of noise."),
                            kind: SettingKind::IntRange { range: 1..=5 },
                            id: setting_id::LUMA_NOISE_DETAIL,
                        },
                    ],
                },
                id: setting_id::LUMA_NOISE,
            },
            SettingDescriptor {
                label: "Chroma noise",
                description: Some("Noise applied to the chrominance (color) signal."),
                kind: SettingKind::Group {
                    children: vec![
                        SettingDescriptor {
                            label: "Intensity",
                            description: Some("Intensity of the noise."),
                            kind: SettingKind::Percentage { logarithmic: true },
                            id: setting_id::CHROMA_NOISE_INTENSITY,
                        },
                        SettingDescriptor {
                            label: "Frequency",
                            description: Some("Base wavelength, in pixels, of the noise."),
                            kind: SettingKind::FloatRange {
                                range: 0.0..=0.5,
                                logarithmic: false,
                            },
                            id: setting_id::CHROMA_NOISE_FREQUENCY,
                        },
                        SettingDescriptor {
                            label: "Detail",
                            description: Some("Octaves of noise."),
                            kind: SettingKind::IntRange { range: 1..=5 },
                            id: setting_id::CHROMA_NOISE_DETAIL,
                        },
                    ],
                },
                id: setting_id::CHROMA_NOISE,
            },
            SettingDescriptor {
                label: "Chroma phase error",
                description: Some("Phase error for the chrominance (color) signal."),
                kind: SettingKind::Percentage { logarithmic: false },
                id: setting_id::CHROMA_PHASE_ERROR,
            },
            SettingDescriptor {
                label: "Chroma phase noise",
                description: Some(
                    "Noise applied per-scanline to the phase of the chrominance (color) signal.",
                ),
                kind: SettingKind::Percentage { logarithmic: true },
                id: setting_id::CHROMA_PHASE_NOISE_INTENSITY,
            },
            SettingDescriptor {
                label: "Chroma delay (horizontal)",
                description: Some("Horizontal offset of the chrominance (color) signal."),
                kind: SettingKind::FloatRange {
                    range: -40.0..=40.0,
                    logarithmic: false,
                },
                id: setting_id::CHROMA_DELAY_HORIZONTAL,
            },
            SettingDescriptor {
                label: "Chroma delay (vertical)",
                description: Some(
                    "Vertical offset of the chrominance (color) signal. Usually increases with \
                     VHS generation loss.",
                ),
                kind: SettingKind::IntRange { range: -20..=20 },
                id: setting_id::CHROMA_DELAY_VERTICAL,
            },
            SettingDescriptor {
                label: "VHS emulation",
                description: None,
                kind: SettingKind::Group {
                    children: vec![
                        SettingDescriptor {
                            label: "Tape speed",
                            description: Some(
                                "Emulate cutoff of high-frequency data at various VHS recording \
                                 speeds.",
                            ),
                            kind: SettingKind::Enumeration {
                                options: vec![
                                    MenuItem {
                                        label: "SP (Standard Play)",
                                        description: None,
                                        index: VHSTapeSpeed::SP as u32,
                                    },
                                    MenuItem {
                                        label: "LP (Long Play)",
                                        description: None,
                                        index: VHSTapeSpeed::LP as u32,
                                    },
                                    MenuItem {
                                        label: "EP (Extended Play)",
                                        description: None,
                                        index: VHSTapeSpeed::EP as u32,
                                    },
                                    MenuItem {
                                        label: "None",
                                        description: None,
                                        index: 0,
                                    },
                                ],
                            },
                            id: setting_id::VHS_TAPE_SPEED,
                        },
                        SettingDescriptor {
                            label: "Chroma loss",
                            description: Some(
                                "Chance that the chrominance (color) signal is completely lost in \
                                 each scanline.",
                            ),
                            kind: SettingKind::Percentage { logarithmic: true },
                            id: setting_id::VHS_CHROMA_LOSS,
                        },
                        SettingDescriptor {
                            label: "Sharpen",
                            description: Some(
                                "Sharpening of the image, as done by some VHS decks.",
                            ),
                            kind: SettingKind::Group {
                                children: vec![
                                    SettingDescriptor {
                                        label: "Intensity",
                                        description: Some("Amount of sharpening to apply."),
                                        kind: SettingKind::FloatRange {
                                            range: 0.0..=5.0,
                                            logarithmic: false,
                                        },
                                        id: setting_id::VHS_SHARPEN_INTENSITY,
                                    },
                                    SettingDescriptor {
                                        label: "Frequency",
                                        description: Some(
                                            "Frequency / radius of the sharpening, relative to \
                                             the tape speed's cutoff frequency.",
                                        ),
                                        kind: SettingKind::FloatRange {
                                            range: 0.5..=4.0,
                                            logarithmic: false,
                                        },
                                        id: setting_id::VHS_SHARPEN_FREQUENCY,
                                    },
                                ],
                            },
                            id: setting_id::VHS_SHARPEN_ENABLED,
                        },
                        SettingDescriptor {
                            label: "Edge wave",
                            description: Some("Horizontal waving of the image."),
                            kind: SettingKind::Group {
                                children: vec![
                                    SettingDescriptor {
                                        label: "Intensity",
                                        description: Some(
                                            "Horizontal waving of the image, in pixels.",
                                        ),
                                        kind: SettingKind::FloatRange {
                                            range: 0.0..=20.0,
                                            logarithmic: false,
                                        },
                                        id: setting_id::VHS_EDGE_WAVE_INTENSITY,
                                    },
                                    SettingDescriptor {
                                        label: "Speed",
                                        description: Some(
                                            "Speed at which the horizontal waving occurs.",
                                        ),
                                        kind: SettingKind::FloatRange {
                                            range: 0.0..=10.0,
                                            logarithmic: false,
                                        },
                                        id: setting_id::VHS_EDGE_WAVE_SPEED,
                                    },
                                    SettingDescriptor {
                                        label: "Frequency",
                                        description: Some(
                                            "Base wavelength for the horizontal waving.",
                                        ),
                                        kind: SettingKind::FloatRange {
                                            range: 0.0..=0.5,
                                            logarithmic: false,
                                        },
                                        id: setting_id::VHS_EDGE_WAVE_FREQUENCY,
                                    },
                                    SettingDescriptor {
                                        label: "Detail",
                                        description: Some("Octaves of noise for the waves."),
                                        kind: SettingKind::IntRange { range: 1..=5 },
                                        id: setting_id::VHS_EDGE_WAVE_DETAIL,
                                    },
                                ],
                            },
                            id: setting_id::VHS_EDGE_WAVE_ENABLED,
                        },
                    ],
                },
                id: setting_id::VHS_SETTINGS,
            },
            SettingDescriptor {
                label: "Vertically blend chroma",
                description: Some(
                    "Vertically blend each scanline's chrominance with the scanline above it.",
                ),
                kind: SettingKind::Boolean,
                id: setting_id::CHROMA_VERT_BLEND,
            },
            SettingDescriptor {
                label: "Chroma low-pass out",
                description: Some("Apply a low-pass filter to the output chroma signal."),
                kind: SettingKind::Enumeration {
                    options: vec![
                        MenuItem {
                            label: "Full",
                            description: Some("Full-intensity low-pass filter."),
                            index: ChromaLowpass::Full as u32,
                        },
                        MenuItem {
                            label: "Light",
                            description: Some("Less intense low-pass filter."),
                            index: ChromaLowpass::Light as u32,
                        },
                        MenuItem {
                            label: "None",
                            description: Some("No low-pass filter."),
                            index: ChromaLowpass::None as u32,
                        },
                    ],
                },
                id: setting_id::CHROMA_LOWPASS_OUT,
            },
            SettingDescriptor {
                label: "Scale",
                description: Some("Scale the effect by these factors."),
                kind: SettingKind::Group {
                    children: vec![
                        SettingDescriptor {
                            label: "Horizontal scale",
                            description: Some(
                                "Horizontally scale the effect by this amount. For 480p video, \
                                 leave this at 1.0 for the most physically-accurate result.",
                            ),
                            kind: SettingKind::FloatRange {
                                range: 0.125..=8.0,
                                logarithmic: false,
                            },
                            id: setting_id::HORIZONTAL_SCALE,
                        },
                        SettingDescriptor {
                            label: "Vertical scale",
                            description: Some(
                                "Vertically scale the effect by this amount. You should probably \
                                 leave this at 1.0.",
                            ),
                            kind: SettingKind::FloatRange {
                                range: 0.125..=8.8,
                                logarithmic: false,
                            },
                            id: setting_id::VERTICAL_SCALE,
                        },
                        SettingDescriptor {
                            label: "Scale with video size",
                            description: Some(
                                "Multiply the scaling factors by the video's height. Prefer \
                                 scaling the input video to 480p instead, which gives much more \
                                 accurate-looking results.",
                            ),
                            kind: SettingKind::Boolean,
                            id: setting_id::SCALE_WITH_VIDEO_SIZE,
                        },
                    ],
                },
                id: setting_id::SCALE_SETTINGS,
            },
        ]
        .into_boxed_slice()
    }

    fn legacy_value() -> Self {
        Self {
            filter_type: FilterType::ConstantK, // added in v0.5.9
            luma_smear: 0.0,                    // added in v0.5.2
            head_switching: SettingsBlock {
                enabled: true,
                settings: HeadSwitchingSettings {
                    mid_line: SettingsBlock {
                        // added in v0.7.0
                        enabled: false,
                        settings: Default::default(),
                    },
                    ..Default::default()
                },
            },
            composite_noise: SettingsBlock {
                enabled: true,
                settings: FbmNoiseSettings {
                    frequency: 0.25, // added in v0.7.0
                    detail: 1,       // added in v0.7.0
                    intensity: 0.05,
                },
            },
            luma_noise: SettingsBlock {
                enabled: false,
                settings: FbmNoiseSettings {
                    frequency: 0.5,
                    intensity: 0.01,
                    detail: 1,
                },
            }, // added in v0.7.0
            chroma_noise: SettingsBlock {
                enabled: true,
                settings: FbmNoiseSettings {
                    frequency: 0.05, // added in v0.5.1
                    detail: 1,       // added in v0.5.1
                    intensity: 0.1,
                },
            },
            vhs_settings: SettingsBlock {
                enabled: true,
                settings: VHSSettings {
                    edge_wave: SettingsBlock {
                        enabled: true,
                        settings: VHSEdgeWaveSettings {
                            frequency: 0.05, // added in v0.4.0
                            detail: 1,       // added in v0.4.0
                            ..Default::default()
                        },
                    },
                    sharpen: SettingsBlock {
                        enabled: true,
                        settings: VHSSharpenSettings {
                            frequency: 1.0, // added in v0.5.10
                            ..Default::default()
                        },
                    },
                    ..Default::default()
                },
            },
            ..Default::default()
        }
    }
}

impl SettingsList<NtscEffect> {
    pub fn from_json(&self, json: &str) -> Result<NtscEffect, ParseSettingsError> {
        let parsed_map = parse_json(json)?;

        if parsed_map.contains_key("_composite_preemphasis") {
            return Self::from_ntscqt_json(&parsed_map);
        }

        let version = parsed_map
            .get_and_expect::<f32>("version")?
            .ok_or(ParseSettingsError::MissingField { field: "version" })?;
        if version != 1.0 {
            return Err(ParseSettingsError::UnsupportedVersion { version });
        }

        let mut dst_settings = NtscEffect::legacy_value();
        Self::settings_from_json(&parsed_map, &self.setting_descriptors, &mut dst_settings)?;

        Ok(dst_settings)
    }

    fn from_ntscqt_json(
        json: &SortedMap<Cow<'_, str>, JsonValue>,
    ) -> Result<NtscEffect, ParseSettingsError> {
        let mut settings = NtscEffect::default();
        settings.use_field = UseField::Upper;
        settings.filter_type = FilterType::ConstantK;
        settings.input_luma_filter = LumaLowpass::Box;
        settings.chroma_lowpass_in = {
            let in_lowpass = json
                .get_and_expect::<bool>("_composite_in_chroma_lowpass")?
                .unwrap_or_default();
            if in_lowpass {
                ChromaLowpass::Full
            } else {
                ChromaLowpass::None
            }
        };
        settings.chroma_demodulation = ChromaDemodulationFilter::Box;
        settings.luma_smear = 0.0;
        settings.composite_sharpening = json
            .get_and_expect::<f32>("_composite_preemphasis")?
            .unwrap_or_default();
        settings.head_switching = SettingsBlock {
            enabled: json
                .get_and_expect::<bool>("_vhs_head_switching")?
                .unwrap_or_default(),
            settings: HeadSwitchingSettings {
                height: 6,
                offset: 0,
                horiz_shift: 6.0,
                mid_line: SettingsBlock {
                    enabled: false,
                    settings: Default::default(),
                },
            },
        };
        settings.tracking_noise.enabled = false;
        settings.composite_noise = SettingsBlock {
            enabled: true,
            settings: FbmNoiseSettings {
                frequency: 0.1,
                intensity: json
                    .get_and_expect::<f32>("_video_noise")?
                    .unwrap_or_default()
                    / 50000.0,
                detail: 3,
            },
        };
        settings.ringing = SettingsBlock {
            enabled: true,
            settings: {
                let ringing2 = json
                    .get_and_expect::<bool>("_enable_ringing2")?
                    .unwrap_or_default();
                if ringing2 {
                    let power = json
                        .get_and_expect::<f32>("_ringing_power")?
                        .unwrap_or_default();
                    let shift = json
                        .get_and_expect::<f32>("_ringing_shift")?
                        .unwrap_or_default();

                    let frequency = ((shift * 0.75) + 0.25).clamp(0.0, 1.0);

                    RingingSettings {
                        frequency,
                        power: 6.0,
                        intensity: power * 0.1,
                    }
                } else {
                    RingingSettings {
                        frequency: json.get_and_expect::<f32>("_ringing")?.unwrap_or_default()
                            / 3.0,
                        power: 5.0,
                        intensity: 2.0,
                    }
                }
            },
        };
        settings.luma_noise.enabled = false;
        settings.chroma_noise = SettingsBlock {
            enabled: true,
            settings: FbmNoiseSettings {
                frequency: 0.2,
                intensity: (json
                    .get_and_expect::<f32>("_video_chroma_noise")?
                    .unwrap_or_default()
                    * (0.4 / 16384.0)),
                detail: 1,
            },
        };
        settings.snow_intensity = 0.0;
        settings.chroma_phase_noise_intensity = json
            .get_and_expect::<f32>("_video_chroma_phase_noise")?
            .unwrap_or_default()
            * 0.005;
        settings.chroma_phase_error = 0.0;
        settings.chroma_delay_horizontal = json
            .get_and_expect::<f32>("_color_bleed_horiz")?
            .unwrap_or_default();
        settings.chroma_delay_vertical = json
            .get_and_expect::<i32>("_color_bleed_vert")?
            .unwrap_or_default();
        settings.vhs_settings = SettingsBlock {
            enabled: json
                .get_and_expect::<bool>("_emulating_vhs")?
                .unwrap_or_default(),
            settings: VHSSettings {
                tape_speed: match json.get_and_expect::<f32>("_output_vhs_tape_speed")? {
                    Some(1.0) => VHSTapeSpeed::LP,
                    Some(2.0) => VHSTapeSpeed::EP,
                    _ => VHSTapeSpeed::SP,
                },
                chroma_loss: json
                    .get_and_expect::<f32>("_video_chroma_loss")?
                    .unwrap_or_default()
                    / 100000.0,
                sharpen: SettingsBlock {
                    enabled: true,
                    settings: VHSSharpenSettings {
                        intensity: json
                            .get_and_expect::<f32>("_vhs_out_sharpen")?
                            .unwrap_or_default(),
                        frequency: 1.0,
                    },
                },
                edge_wave: SettingsBlock {
                    enabled: true,
                    settings: VHSEdgeWaveSettings {
                        intensity: (json
                            .get_and_expect::<f32>("_vhs_edge_wave")?
                            .unwrap_or_default()
                            * 0.5),
                        speed: 10.0,
                        frequency: 0.125,
                        detail: 3,
                    },
                },
            },
        };
        settings.chroma_vert_blend = json
            .get_and_expect::<bool>("_vhs_chroma_vert_blend")?
            .unwrap_or_default();
        settings.chroma_lowpass_out = {
            let chroma_lowpass_out = json
                .get_and_expect::<bool>("_composite_out_chroma_lowpass")?
                .unwrap_or_default();
            let chroma_lowpass_out_lite = json
                .get_and_expect::<bool>("_composite_out_chroma_lowpass_lite")?
                .unwrap_or_default();
            match (chroma_lowpass_out, chroma_lowpass_out_lite) {
                (true, false) => ChromaLowpass::Full,
                (true, true) => ChromaLowpass::Light,
                _ => ChromaLowpass::None,
            }
        };
        settings.scale = SettingsBlock {
            enabled: false,
            settings: ScaleSettings::default(),
        };

        Ok(settings)
    }
}
