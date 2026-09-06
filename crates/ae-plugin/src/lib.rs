#![cfg(any(windows, target_os = "macos"))]

mod handle;
mod window_handle;

use std::{
    borrow::BorrowMut,
    fs::File,
    mem::{self, MaybeUninit},
};

use after_effects::{self as ae};
use handle::SliceHandle;
use ntsc_rs::{
    NtscEffect,
    ctx::Context,
    settings::{
        EnumValue, SettingDescriptor, SettingField, SettingID, SettingKind, Settings, SettingsList,
        standard::UseField,
    },
    yiq_fielding::{
        self, AfterEffectsU16, Bgrx, BlitInfo, DeinterlaceMode, Xrgb, YiqField, YiqView,
    },
};
use window_handle::WindowAndDisplayHandle;

struct Plugin {
    settings: SettingsList<NtscEffect>,
    ctx: Context,
}

impl Default for Plugin {
    fn default() -> Self {
        Self {
            settings: SettingsList::<NtscEffect>::new(),
            ctx: Context::new(),
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
enum ParamID {
    Param(i32),
    GroupStart(i32),
    GroupEnd(i32),
    LoadPresetButton,
    SavePresetButton,
}

trait IDExt {
    fn ae_id(&self) -> i32;
}

impl<T: Settings> IDExt for SettingID<T> {
    fn ae_id(&self) -> i32 {
        self.id as i32 + 1
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NtscrsPixelFormat {
    Xrgb8,
    Xrgb16AE,
    Xrgb32f,
    Bgrx8,
    Bgrx16,
    Bgrx32f,
}

impl TryFrom<PixelFormat> for NtscrsPixelFormat {
    type Error = Error;

    fn try_from(value: PixelFormat) -> Result<Self, Self::Error> {
        match value {
            PixelFormat::Argb32 => Ok(Self::Xrgb8),
            PixelFormat::Argb64 => Ok(Self::Xrgb16AE),
            PixelFormat::Argb128 => Ok(Self::Xrgb32f),
            PixelFormat::Bgra32 => Ok(Self::Bgrx8),
            _ => Err(Error::BadCallbackParameter),
        }
    }
}

impl TryFrom<pr::PixelFormat> for NtscrsPixelFormat {
    type Error = Error;

    fn try_from(value: pr::PixelFormat) -> Result<Self, Self::Error> {
        match value {
            pr::PixelFormat::Bgra4444_8u => Ok(Self::Bgrx8),
            pr::PixelFormat::Bgra4444_16u => Ok(Self::Bgrx16),
            pr::PixelFormat::Bgra4444_32f => Ok(Self::Bgrx32f),
            _ => Err(Error::BadCallbackParameter),
        }
    }
}

const LOG_SLIDER_BASE: f64 = 100.0;

/// Maps a slider value to a roughly logarithmic curve, keeping the min and max values where they are.
fn map_logarithmic(value: f64, min: f64, max: f64, base: f64) -> f64 {
    (max - min) * ((f64::powf(base, (value - min) / (max - min)) - 1.0) / (base - 1.0)) + min
}

/// Map it back. This is used for converting default slider values.
fn map_logarithmic_inverse(value: f64, min: f64, max: f64, base: f64) -> f64 {
    f64::log(((value - min) / (max - min)) * (base - 1.0) + 1.0, base) * (max - min) + min
}

ae::define_effect!(Plugin, (), ParamID);

impl AdobePluginGlobal for Plugin {
    fn params_setup(
        &self,
        params: &mut Parameters<ParamID>,
        _in_data: InData,
        _out_data: OutData,
    ) -> Result<(), Error> {
        params.add_customized(
            ParamID::LoadPresetButton,
            "",
            ae::ButtonDef::setup(|def| {
                def.set_label("Load Preset...");
            }),
            |p| {
                p.set_flag(ParamFlag::START_COLLAPSED, true);
                p.set_flag(ParamFlag::SUPERVISE, true);
                -1
            },
        )?;

        params.add_customized(
            ParamID::SavePresetButton,
            "",
            ae::ButtonDef::setup(|def| {
                def.set_label("Save Preset...");
            }),
            |p| {
                p.set_flag(ParamFlag::START_COLLAPSED, true);
                p.set_flag(ParamFlag::SUPERVISE, true);
                -1
            },
        )?;

        Self::map_params(
            params,
            &self.settings.setting_descriptors,
            &NtscEffect::default(),
            &NtscEffect::legacy_value(),
        )?;

        Ok(())
    }

    fn handle_command(
        &mut self,
        command: Command,
        in_data: InData,
        out_data: OutData,
        params: &mut Parameters<ParamID>,
    ) -> Result<(), Error> {
        match command {
            Command::GlobalSetup => self.global_setup(in_data, out_data, params)?,
            Command::About => self.about(in_data, out_data)?,
            Command::Render {
                in_layer,
                out_layer,
            } => self.legacy_render(in_data, out_data, in_layer, out_layer, params)?,
            Command::SmartPreRender { extra } => self.pre_render(in_data, out_data, extra)?,
            Command::SmartRender { extra } => {
                self.smart_render(in_data, out_data, extra, params)?
            }
            Command::UpdateParamsUi => {
                Self::update_controls_disabled(params, &self.settings.setting_descriptors, true)?
            }
            Command::UserChangedParam { param_index } => {
                self.handle_param_callback(params, in_data, out_data, param_index)?
            }
            Command::GetFlattenedSequenceData => {}
            _ => {}
        }

        Ok(())
    }
}

fn ceil_div(a: i32, b: i32) -> i32 {
    (a / b) + (a % b != 0) as i32
}

fn ceil_mul_rational(n: i32, scale: RationalScale) -> i32 {
    ceil_div(n * scale.num, scale.den as i32)
}

unsafe fn transmute_slice<T, U>(slice: &[T]) -> &[U] {
    let src_size = mem::size_of::<T>();
    let dst_size = mem::size_of::<U>();
    let new_len = if dst_size > src_size {
        slice.len() / (dst_size / src_size)
    } else {
        slice.len() * (src_size / dst_size)
    };
    unsafe { std::slice::from_raw_parts(slice.as_ptr() as _, new_len) }
}

unsafe fn transmute_slice_mut<T, U>(slice: &mut [T]) -> &mut [U] {
    let src_size = mem::size_of::<T>();
    let dst_size = mem::size_of::<U>();
    let new_len = if dst_size > src_size {
        slice.len() / (dst_size / src_size)
    } else {
        slice.len() * (src_size / dst_size)
    };
    unsafe { std::slice::from_raw_parts_mut(slice.as_mut_ptr() as _, new_len) }
}

impl Plugin {
    fn global_setup(
        &self,
        in_data: InData,
        mut _out_data: OutData,
        _params: &mut Parameters<ParamID>,
    ) -> Result<(), Error> {
        let is_premiere = in_data.is_premiere();
        if is_premiere {
            let pf = suites::PixelFormat::new()?;
            pf.clear_supported_pixel_formats(in_data.effect_ref())?;
            pf.add_supported_pixel_format(in_data.effect_ref(), pr::PixelFormat::Bgra4444_8u)?;
            pf.add_supported_pixel_format(in_data.effect_ref(), pr::PixelFormat::Bgra4444_16u)?;
            pf.add_supported_pixel_format(in_data.effect_ref(), pr::PixelFormat::Bgra4444_32f)?;
        }

        Ok(())
    }

    fn about(&self, _in_data: InData, mut out_data: OutData) -> Result<(), Error> {
        const DESCRIPTION: &str = "Analog TV and VHS emulation.";
        out_data.set_return_msg(
            format!(
                "NTSC-rs {}.{}.{}\r\r{DESCRIPTION}",
                env!("EFFECT_VERSION_MAJOR"),
                env!("EFFECT_VERSION_MINOR"),
                env!("EFFECT_VERSION_PATCH")
            )
            .as_str(),
        );
        Ok(())
    }

    fn pre_render(
        &self,
        in_data: InData,
        _out_data: OutData,
        mut extra: PreRenderExtra,
    ) -> Result<(), Error> {
        let mut req = extra.output_request();
        req.preserve_rgb_of_zero_alpha = 1;

        // I don't know anymore. I thought the way to ask for really all the pixels (yes I promise I need all of them for
        // the effect to work!) was to set result_rect with the helpfully-provided ref_width and ref_height properties from
        // the checkout result. Apparently, that's too *late*, and we need to tell the request itself before we check out
        // the layer that we're planning on checking out these pixels. No thanks to the SmartFX documentation, which fails
        // to mention which parameters have an effect on the input pixel region we're allowed to access, which ones we're
        // even *allowed to modify* (it mixes input and output parameters in the same structs!), and half the time, what
        // coordinate space we're even working in and whether downsampling is taken into account. I'm 70% sure this is
        // *still* wrong somehow, but it seems to give the correct results. I hope whoever designed the SmartFX API and
        // *especially* whoever wrote the paltry and inadequate documentation for it are cursed with wet socks for eternity.
        req.rect.left = 0;
        req.rect.right = ceil_mul_rational(in_data.width(), in_data.downsample_x());
        req.rect.top = 0;
        req.rect.bottom = ceil_mul_rational(in_data.height(), in_data.downsample_y());

        let in_res = extra.callbacks().checkout_layer(
            0,
            0,
            &req,
            in_data.current_time(),
            in_data.time_step(),
            in_data.time_scale(),
        )?;

        // Completely ignore previous bounds, and just use the layer bounds (for adjustment layers and shape layers, these
        // are the comp bounds). This was surprisingly tricky to figure out--you'd think lots of effects would want to do
        // this.
        let out_width = ceil_mul_rational(in_res.ref_width, in_data.downsample_x());
        let out_height = ceil_mul_rational(in_res.ref_height, in_data.downsample_y());

        let constrained_rect = Rect {
            left: 0,
            top: 0,
            right: out_width,
            bottom: out_height,
        };

        extra.set_result_rect(constrained_rect);
        extra.set_max_result_rect(constrained_rect);
        extra.set_returns_extra_pixels(true);

        Ok(())
    }

    fn legacy_render(
        &self,
        in_data: InData,
        out_data: OutData,
        in_layer: Layer,
        out_layer: Layer,
        params: &mut Parameters<ParamID>,
    ) -> Result<(), Error> {
        if !in_data.is_premiere() {
            // We don't support non-SmartFX unless it's Premiere
            return Err(Error::BadCallbackParameter);
        }

        if in_layer.width() != out_layer.width() || in_layer.height() != out_layer.height() {
            // I haven't tested what kind of coordinate space transformations need to happen here
            return Err(Error::BadCallbackParameter);
        }

        let in_pixel_format = NtscrsPixelFormat::try_from(in_layer.pr_pixel_format()?)?;
        let out_pixel_format = NtscrsPixelFormat::try_from(in_layer.pr_pixel_format()?)?;

        self.do_render(
            in_data,
            in_layer,
            out_data,
            out_layer,
            in_pixel_format,
            out_pixel_format,
            params,
        )?;

        Ok(())
    }

    fn smart_render(
        &self,
        in_data: InData,
        out_data: OutData,
        extra: SmartRenderExtra,
        params: &mut Parameters<ParamID>,
    ) -> Result<(), Error> {
        let Some(input_world) = extra.callbacks().checkout_layer_pixels(0)? else {
            return Ok(());
        };
        let Some(output_world) = extra.callbacks().checkout_output()? else {
            return Ok(());
        };

        let src_pixel_format = input_world.pixel_format()?;
        let dst_pixel_format = output_world.pixel_format()?;

        self.do_render(
            in_data,
            input_world,
            out_data,
            output_world,
            src_pixel_format.try_into()?,
            dst_pixel_format.try_into()?,
            params,
        )
    }

    fn do_render(
        &self,
        in_data: InData,
        in_layer: Layer,
        _out_data: OutData,
        mut out_layer: Layer,
        in_pixel_format: NtscrsPixelFormat,
        out_pixel_format: NtscrsPixelFormat,
        params: &mut Parameters<ParamID>,
    ) -> Result<(), Error> {
        let effect: NtscEffect = self.apply_settings(params)?;

        let frame_num = in_data.current_frame() as usize;

        let yiq_field = match effect.use_field {
            UseField::Alternating => {
                if frame_num & 1 == 1 {
                    YiqField::Upper
                } else {
                    YiqField::Lower
                }
            }
            UseField::Upper => YiqField::Upper,
            UseField::Lower => YiqField::Lower,
            UseField::Both => YiqField::Both,
            UseField::InterleavedUpper => YiqField::InterleavedUpper,
            UseField::InterleavedLower => YiqField::InterleavedLower,
        };

        let out_buf_size =
            YiqView::buf_length_for((out_layer.width(), out_layer.height()), yiq_field);
        let mut out_handle = SliceHandle::<f32>::new(out_buf_size, 0.0)?;
        let mut locked = out_handle.lock()?;
        let out_buf: &mut [f32] = locked.borrow_mut();
        let mut view =
            YiqView::from_parts(out_buf, (out_layer.width(), out_layer.height()), yiq_field);

        let src_row_bytes = in_layer.row_bytes();
        let (src_row_bytes, src_flip_y) = if src_row_bytes > 0 {
            (src_row_bytes as usize, false)
        } else {
            (-src_row_bytes as usize, true)
        };

        let dst_row_bytes = out_layer.row_bytes();
        let (dst_row_bytes, dst_flip_y) = if dst_row_bytes > 0 {
            (dst_row_bytes as usize, false)
        } else {
            (-dst_row_bytes as usize, true)
        };

        let input_origin = in_layer.origin();
        let src_offset = (
            input_origin.h.max(0) as usize,
            input_origin.v.max(0) as usize,
        );

        let src_blit_info = BlitInfo {
            rect: yiq_fielding::Rect::new(
                0,
                0,
                in_layer.height().min(out_layer.height() - src_offset.1),
                in_layer.width().min(out_layer.width() - src_offset.0),
            ),
            destination: src_offset,
            row_bytes: src_row_bytes,
            other_buffer_height: in_layer.height(),
            flip_y: src_flip_y,
        };

        let dst_blit_info = BlitInfo {
            rect: yiq_fielding::Rect::new(0, 0, out_layer.height(), out_layer.width()),
            destination: (0, 0),
            row_bytes: dst_row_bytes,
            other_buffer_height: out_layer.height(),
            flip_y: dst_flip_y,
        };

        match in_pixel_format {
            NtscrsPixelFormat::Xrgb8 => unsafe {
                let data = transmute_slice::<u8, MaybeUninit<u8>>(in_layer.buffer());
                view.set_from_strided_buffer_maybe_uninit::<Xrgb, u8, _>(
                    &self.ctx,
                    data,
                    src_blit_info,
                    (),
                );
            },
            NtscrsPixelFormat::Xrgb16AE => unsafe {
                let data = transmute_slice::<u8, MaybeUninit<AfterEffectsU16>>(in_layer.buffer());
                view.set_from_strided_buffer_maybe_uninit::<Xrgb, AfterEffectsU16, _>(
                    &self.ctx,
                    data,
                    src_blit_info,
                    (),
                );
            },
            NtscrsPixelFormat::Xrgb32f => unsafe {
                let data = transmute_slice::<u8, MaybeUninit<f32>>(in_layer.buffer());
                view.set_from_strided_buffer_maybe_uninit::<Xrgb, f32, _>(
                    &self.ctx,
                    data,
                    src_blit_info,
                    (),
                );
            },
            NtscrsPixelFormat::Bgrx8 => unsafe {
                let data = transmute_slice::<u8, MaybeUninit<u8>>(in_layer.buffer());
                view.set_from_strided_buffer_maybe_uninit::<Bgrx, u8, _>(
                    &self.ctx,
                    data,
                    src_blit_info,
                    (),
                );
            },
            NtscrsPixelFormat::Bgrx16 => unsafe {
                let data = transmute_slice::<u8, MaybeUninit<u16>>(in_layer.buffer());
                view.set_from_strided_buffer_maybe_uninit::<Bgrx, u16, _>(
                    &self.ctx,
                    data,
                    src_blit_info,
                    (),
                );
            },
            NtscrsPixelFormat::Bgrx32f => unsafe {
                let data = transmute_slice::<u8, MaybeUninit<f32>>(in_layer.buffer());
                view.set_from_strided_buffer_maybe_uninit::<Bgrx, f32, _>(
                    &self.ctx,
                    data,
                    src_blit_info,
                    (),
                );
            },
        }

        let scale_factors: [f32; 2] = if effect
            .scale
            .as_option()
            .is_some_and(|scale| scale.scale_with_video_size)
        {
            [1.0, 1.0]
        } else {
            [in_data.downsample_x(), in_data.downsample_y()].map(|factor| factor.into())
        };
        effect.apply_effect_to_yiq(&self.ctx, &mut view, frame_num, scale_factors);

        match out_pixel_format {
            NtscrsPixelFormat::Xrgb8 => {
                let data =
                    unsafe { transmute_slice_mut::<u8, MaybeUninit<u8>>(out_layer.buffer_mut()) };
                view.write_to_strided_buffer_maybe_uninit::<Xrgb, u8, _>(
                    &self.ctx,
                    data,
                    dst_blit_info,
                    DeinterlaceMode::Bob,
                    (),
                );
            }
            NtscrsPixelFormat::Xrgb16AE => {
                let data = unsafe {
                    transmute_slice_mut::<u8, MaybeUninit<AfterEffectsU16>>(out_layer.buffer_mut())
                };
                view.write_to_strided_buffer_maybe_uninit::<Xrgb, AfterEffectsU16, _>(
                    &self.ctx,
                    data,
                    dst_blit_info,
                    DeinterlaceMode::Bob,
                    (),
                );
            }
            NtscrsPixelFormat::Xrgb32f => {
                let data =
                    unsafe { transmute_slice_mut::<u8, MaybeUninit<f32>>(out_layer.buffer_mut()) };
                view.write_to_strided_buffer_maybe_uninit::<Xrgb, f32, _>(
                    &self.ctx,
                    data,
                    dst_blit_info,
                    DeinterlaceMode::Bob,
                    (),
                );
            }
            NtscrsPixelFormat::Bgrx8 => {
                let data =
                    unsafe { transmute_slice_mut::<u8, MaybeUninit<u8>>(out_layer.buffer_mut()) };
                view.write_to_strided_buffer_maybe_uninit::<Bgrx, u8, _>(
                    &self.ctx,
                    data,
                    dst_blit_info,
                    DeinterlaceMode::Bob,
                    (),
                );
            }
            NtscrsPixelFormat::Bgrx16 => {
                let data =
                    unsafe { transmute_slice_mut::<u8, MaybeUninit<u16>>(out_layer.buffer_mut()) };
                view.write_to_strided_buffer_maybe_uninit::<Bgrx, u16, _>(
                    &self.ctx,
                    data,
                    dst_blit_info,
                    DeinterlaceMode::Bob,
                    (),
                );
            }
            NtscrsPixelFormat::Bgrx32f => {
                let data =
                    unsafe { transmute_slice_mut::<u8, MaybeUninit<f32>>(out_layer.buffer_mut()) };
                view.write_to_strided_buffer_maybe_uninit::<Bgrx, f32, _>(
                    &self.ctx,
                    data,
                    dst_blit_info,
                    DeinterlaceMode::Bob,
                    (),
                );
            }
        }

        Ok(())
    }

    fn update_controls_disabled(
        params: &mut Parameters<ParamID>,
        descriptors: &[SettingDescriptor<NtscEffect>],
        enabled: bool,
    ) -> Result<(), Error> {
        for descriptor in descriptors {
            if let SettingKind::Group { children, .. } = &descriptor.kind {
                let group_enabled = params
                    .get(ParamID::Param(descriptor.id.ae_id()))?
                    .as_checkbox()?
                    .value();
                Self::update_controls_disabled(params, children, enabled && group_enabled)?;
            }
            if let Ok(p) = params.get(ParamID::Param(descriptor.id.ae_id())) {
                let was_enabled = !p.ui_flags().contains(ParamUIFlags::DISABLED);

                if was_enabled != enabled {
                    // The parameter definition must be cloned--modifying it in-place will result in a crash
                    let mut p = p.clone();
                    p.set_ui_flag(ParamUIFlags::DISABLED, !enabled);
                    p.update_param_ui()?;
                }
            }
        }

        Ok(())
    }

    fn map_params(
        params: &mut Parameters<ParamID>,
        descriptors: &[SettingDescriptor<NtscEffect>],
        default_settings: &NtscEffect,
        legacy_default_settings: &NtscEffect,
    ) -> Result<(), Error> {
        fn get_defaults<T: SettingField + 'static>(
            defaults: &NtscEffect,
            legacy_defaults: &NtscEffect,
            descriptor: &SettingDescriptor<NtscEffect>,
        ) -> Result<[T; 2], Error> {
            Ok([
                defaults
                    .get_field(&descriptor.id)
                    .map_err(|_| Error::BadCallbackParameter)?,
                legacy_defaults
                    .get_field(&descriptor.id)
                    .map_err(|_| Error::BadCallbackParameter)?,
            ])
        }

        for descriptor in descriptors {
            match &descriptor.kind {
                SettingKind::Enumeration { options } => {
                    let [default_idx, legacy_default_idx] = get_defaults::<EnumValue>(
                        default_settings,
                        legacy_default_settings,
                        descriptor,
                    )?
                    .map(|default| {
                        options
                            .iter()
                            .position(|item| item.index == default.0)
                            .unwrap() as i32
                            + 1
                    });
                    params.add_customized(
                        ParamID::Param(descriptor.id.ae_id()),
                        descriptor.label,
                        ae::PopupDef::setup(|p| {
                            p.set_options(&options.iter().map(|o| o.label).collect::<Vec<_>>());
                            p.set_default(default_idx);
                            p.set_value(legacy_default_idx);
                        }),
                        |p| {
                            p.set_id(descriptor.id.ae_id());
                            p.set_flag(ParamFlag::START_COLLAPSED, true);
                            p.set_flag(ParamFlag::USE_VALUE_FOR_OLD_PROJECTS, true);
                            -1
                        },
                    )?;
                }
                SettingKind::Percentage { logarithmic } => {
                    let [default_value, legacy_default_value] = get_defaults::<f32>(
                        default_settings,
                        legacy_default_settings,
                        descriptor,
                    )?
                    .map(|default| match (*logarithmic, default as f64) {
                        (true, v) => {
                            map_logarithmic_inverse(v, 0.0, 1.0, LOG_SLIDER_BASE)
                        }
                        (false, v) => v,
                    } * 100.0);
                    params.add_customized(
                        ParamID::Param(descriptor.id.ae_id()),
                        descriptor.label,
                        ae::FloatSliderDef::setup(|f| {
                            f.set_slider_min(0.0);
                            f.set_valid_min(0.0);
                            f.set_slider_max(100.0);
                            f.set_valid_max(100.0);
                            f.set_default(default_value);
                            f.set_value(legacy_default_value);
                            f.set_display_flags(ValueDisplayFlag::PERCENT);
                            f.set_precision(1);
                        }),
                        |p| {
                            p.set_id(descriptor.id.ae_id());
                            p.set_flag(ParamFlag::START_COLLAPSED, true);
                            p.set_flag(ParamFlag::USE_VALUE_FOR_OLD_PROJECTS, true);
                            -1
                        },
                    )?
                }
                SettingKind::IntRange { range } => {
                    let [default_value, legacy_default_value] =
                        get_defaults::<i32>(default_settings, legacy_default_settings, descriptor)?;
                    params.add_customized(
                        ParamID::Param(descriptor.id.ae_id()),
                        descriptor.label,
                        ae::FloatSliderDef::setup(|f| {
                            f.set_slider_min(*range.start() as f32);
                            f.set_valid_min(*range.start() as f32);
                            f.set_slider_max(*range.end() as f32);
                            f.set_valid_max(*range.end() as f32);
                            f.set_default(default_value as f64);
                            f.set_value(legacy_default_value as f64);
                            f.set_precision(0);
                        }),
                        |p| {
                            p.set_id(descriptor.id.ae_id());
                            p.set_flag(ParamFlag::START_COLLAPSED, true);
                            p.set_flag(ParamFlag::USE_VALUE_FOR_OLD_PROJECTS, true);
                            -1
                        },
                    )?
                }
                SettingKind::FloatRange { range, logarithmic } => {
                    let [default_value, legacy_default_value] =
                        get_defaults::<f32>(default_settings, legacy_default_settings, descriptor)?
                            .map(|default| match (*logarithmic, default as f64) {
                                (true, v) => map_logarithmic_inverse(
                                    v,
                                    *range.start() as f64,
                                    *range.end() as f64,
                                    LOG_SLIDER_BASE,
                                ),
                                (false, v) => v,
                            });
                    params.add_customized(
                        ParamID::Param(descriptor.id.ae_id()),
                        descriptor.label,
                        ae::FloatSliderDef::setup(|f| {
                            f.set_slider_min(*range.start());
                            f.set_valid_min(*range.start());
                            f.set_slider_max(*range.end());
                            f.set_valid_max(*range.end());
                            f.set_default(default_value);
                            f.set_value(legacy_default_value);
                            f.set_precision(2);
                        }),
                        |p| {
                            p.set_id(descriptor.id.ae_id());
                            p.set_flag(ParamFlag::START_COLLAPSED, true);
                            p.set_flag(ParamFlag::USE_VALUE_FOR_OLD_PROJECTS, true);
                            -1
                        },
                    )?
                }
                SettingKind::Boolean => {
                    let [default_value, legacy_default_value] = get_defaults::<bool>(
                        default_settings,
                        legacy_default_settings,
                        descriptor,
                    )?;
                    params.add_customized(
                        ParamID::Param(descriptor.id.ae_id()),
                        descriptor.label,
                        ae::CheckBoxDef::setup(|c| {
                            c.set_default(default_value);
                            c.set_value(legacy_default_value);
                            // The effect will fail to load if we don't set the label (by default it's the null pointer)
                            c.set_label(descriptor.label);
                        }),
                        |p| {
                            p.set_id(descriptor.id.ae_id());
                            p.set_flag(ParamFlag::START_COLLAPSED, true);
                            p.set_flag(ParamFlag::USE_VALUE_FOR_OLD_PROJECTS, true);
                            -1
                        },
                    )?;
                }
                SettingKind::Group { children } => {
                    let descriptor_id = descriptor.id.ae_id();
                    let [default_value, legacy_default_value] = get_defaults::<bool>(
                        default_settings,
                        legacy_default_settings,
                        descriptor,
                    )?;
                    params.add_group(
                        ParamID::GroupStart(descriptor_id),
                        ParamID::GroupEnd(descriptor_id),
                        descriptor.label,
                        false,
                        |g| {
                            g.add_customized(
                                ParamID::Param(descriptor_id),
                                descriptor.label,
                                ae::CheckBoxDef::setup(|c| {
                                    c.set_default(default_value);
                                    c.set_value(legacy_default_value);
                                    c.set_label("Enabled");
                                }),
                                |p| {
                                    p.set_id(descriptor_id);
                                    p.set_flag(ParamFlag::START_COLLAPSED, true);
                                    p.set_flag(ParamFlag::USE_VALUE_FOR_OLD_PROJECTS, true);
                                    -1
                                },
                            )?;
                            Self::map_params(
                                g,
                                children,
                                default_settings,
                                legacy_default_settings,
                            )?;
                            Ok(())
                        },
                    )?;
                }
            }
        }

        Ok(())
    }

    fn get_window_handle(
        &self,
        #[allow(unused_variables)] in_data: &InData,
    ) -> Result<Option<WindowAndDisplayHandle>, Error> {
        #[cfg(windows)]
        {
            let hwnd = if in_data.is_premiere() {
                premiere::suites::Window::new()
                    .map_err(|_| Error::Generic)?
                    .get_main_window() as usize as isize
            } else {
                let utility = ae::aegp::suites::Utility::new()?;
                utility.main_hwnd()? as usize as isize
            };

            Ok(std::num::NonZero::<isize>::new(hwnd).map(|hwnd| unsafe {
                WindowAndDisplayHandle::new(raw_window_handle::Win32WindowHandle::new(hwnd))
            }))
        }

        #[cfg(not(windows))]
        Ok(None)
    }

    fn handle_param_callback(
        &self,
        params: &mut Parameters<ParamID>,
        in_data: InData,
        mut out_data: OutData,
        param_index: usize,
    ) -> Result<(), Error> {
        match params.type_at(param_index) {
            ParamID::LoadPresetButton => {
                let mut dialog = rfd::FileDialog::new().add_filter("ntsc-rs preset", &["json"]);

                // Set the parent window handle on Windows so the user can't interact with the main window while the
                // file picker is open. If they switch projects while it's open, AE will crash when the dialog closes.
                //
                // Seemingly, Premiere won't let us get the window handle in global_setup and always returns null, so we
                // need to do it in this callback.
                if let Some(handle) = self.get_window_handle(&in_data)? {
                    dialog = dialog.set_parent(&handle);
                }

                let Some(preset_path) = dialog.pick_file() else {
                    return Ok(());
                };

                let loaded_preset: Result<_, Box<dyn std::error::Error>> = (|| {
                    let file_contents = std::fs::read_to_string(preset_path)?;
                    Ok(self.settings.from_json(&file_contents)?)
                })();
                let loaded_preset = match loaded_preset {
                    Ok(loaded_preset) => loaded_preset,
                    Err(e) => {
                        out_data.set_error_msg(&format!("Error loading preset: {e}"));
                        return Ok(());
                    }
                };

                Self::update_params_from_settings(
                    &self.settings.setting_descriptors,
                    params,
                    &loaded_preset,
                )?;
            }
            ParamID::SavePresetButton => {
                let mut dialog = rfd::FileDialog::new()
                    .add_filter("ntsc-rs preset", &["json"])
                    .set_file_name("settings.json");

                if let Some(handle) = self.get_window_handle(&in_data)? {
                    dialog = dialog.set_parent(&handle);
                }

                let Some(preset_path) = dialog.save_file() else {
                    return Ok(());
                };

                let effect_settings = self.apply_settings(params)?;
                let res: Result<(), Box<dyn std::error::Error>> = (|| {
                    let mut destination = File::create(preset_path)?;
                    self.settings
                        .write_json_to_io(&effect_settings, &mut destination)?;

                    Ok(())
                })();
                if let Err(e) = res {
                    out_data.set_error_msg(&format!("Error saving preset: {e}"));
                    return Ok(());
                }
            }
            _ => {}
        }

        Ok(())
    }

    fn update_params_from_settings(
        descriptors: &[SettingDescriptor<NtscEffect>],
        params: &mut Parameters<ParamID>,
        settings: &NtscEffect,
    ) -> Result<(), Error> {
        for descriptor in descriptors {
            match &descriptor.kind {
                SettingKind::Enumeration { options, .. } => {
                    let mut param = params.get_mut(ParamID::Param(descriptor.id.ae_id()))?;
                    let mut param = param.as_popup_mut()?;
                    let setting = settings.get_field::<EnumValue>(&descriptor.id).unwrap().0;
                    param.set_value(
                        options
                            .iter()
                            .position(|item| item.index == setting)
                            .unwrap() as i32
                            + 1,
                    );
                    param.set_value_changed();
                }
                SettingKind::Percentage { logarithmic, .. } => {
                    let mut param = params.get_mut(ParamID::Param(descriptor.id.ae_id()))?;
                    let mut param = param.as_float_slider_mut()?;
                    let setting = settings.get_field::<f32>(&descriptor.id).unwrap();
                    param.set_value(
                        if *logarithmic {
                            map_logarithmic_inverse(setting.into(), 0.0, 1.0, LOG_SLIDER_BASE)
                        } else {
                            setting.into()
                        } * 100.0,
                    );
                    param.set_value_changed();
                }
                SettingKind::IntRange { .. } => {
                    let mut param = params.get_mut(ParamID::Param(descriptor.id.ae_id()))?;
                    let mut param = param.as_float_slider_mut()?;
                    let setting = settings.get_field::<i32>(&descriptor.id).unwrap();
                    param.set_value(setting.into());
                    param.set_value_changed();
                }
                SettingKind::FloatRange {
                    range, logarithmic, ..
                } => {
                    let mut param = params.get_mut(ParamID::Param(descriptor.id.ae_id()))?;
                    let mut param = param.as_float_slider_mut()?;
                    let setting = settings.get_field::<f32>(&descriptor.id).unwrap();
                    param.set_value(if *logarithmic {
                        map_logarithmic_inverse(
                            setting.into(),
                            *range.start() as f64,
                            *range.end() as f64,
                            LOG_SLIDER_BASE,
                        )
                    } else {
                        setting.into()
                    });
                    param.set_value_changed();
                }
                SettingKind::Boolean => {
                    let mut param = params.get_mut(ParamID::Param(descriptor.id.ae_id()))?;
                    let mut param = param.as_checkbox_mut()?;
                    let setting = settings.get_field::<bool>(&descriptor.id).unwrap();
                    param.set_value(setting);
                    param.set_value_changed();
                }
                SettingKind::Group { children, .. } => {
                    let mut param = params.get_mut(ParamID::Param(descriptor.id.ae_id()))?;
                    let mut param = param.as_checkbox_mut()?;
                    let setting = settings.get_field::<bool>(&descriptor.id).unwrap();
                    param.set_value(setting);
                    param.set_value_changed();

                    Self::update_params_from_settings(children, params, settings)?;
                }
            }
        }

        Ok(())
    }

    fn apply_settings(&self, params: &mut Parameters<ParamID>) -> Result<NtscEffect, Error> {
        let mut settings = NtscEffect::default();

        fn apply_settings_list(
            descriptors: &[SettingDescriptor<NtscEffect>],
            params: &mut Parameters<ParamID>,
            settings: &mut NtscEffect,
        ) -> Result<(), Error> {
            for descriptor in descriptors {
                match &descriptor.kind {
                    SettingKind::Enumeration { options, .. } => {
                        let selected_item_position = params
                            .get(ParamID::Param(descriptor.id.ae_id()))?
                            .as_popup()?
                            .value()
                            - 1;
                        if selected_item_position < 0 {
                            continue;
                        }
                        let menu_enum_value = options[selected_item_position as usize].index;
                        settings
                            .set_field::<EnumValue>(&descriptor.id, EnumValue(menu_enum_value))
                            .map_err(|_| Error::BadCallbackParameter)?;
                    }
                    SettingKind::Percentage { logarithmic, .. } => {
                        let mut slider_value = params
                            .get(ParamID::Param(descriptor.id.ae_id()))?
                            .as_float_slider()?
                            .value()
                            * 0.01;

                        if *logarithmic {
                            slider_value = map_logarithmic(slider_value, 0.0, 1.0, LOG_SLIDER_BASE);
                        }
                        settings
                            .set_field::<f32>(&descriptor.id, slider_value as f32)
                            .map_err(|_| Error::BadCallbackParameter)?;
                    }
                    SettingKind::IntRange { .. } => {
                        let slider_value = params
                            .get(ParamID::Param(descriptor.id.ae_id()))?
                            .as_float_slider()?
                            .value()
                            .round() as i32;
                        settings
                            .set_field::<i32>(&descriptor.id, slider_value)
                            .map_err(|_| Error::BadCallbackParameter)?;
                    }
                    SettingKind::FloatRange {
                        logarithmic, range, ..
                    } => {
                        let mut slider_value = params
                            .get(ParamID::Param(descriptor.id.ae_id()))?
                            .as_float_slider()?
                            .value();

                        if *logarithmic {
                            slider_value = map_logarithmic(
                                slider_value,
                                *range.start() as f64,
                                *range.end() as f64,
                                LOG_SLIDER_BASE,
                            );
                        }
                        settings
                            .set_field::<f32>(&descriptor.id, slider_value as f32)
                            .map_err(|_| Error::BadCallbackParameter)?;
                    }
                    SettingKind::Boolean => {
                        settings
                            .set_field::<bool>(
                                &descriptor.id,
                                params
                                    .get(ParamID::Param(descriptor.id.ae_id()))?
                                    .as_checkbox()?
                                    .value(),
                            )
                            .map_err(|_| Error::BadCallbackParameter)?;
                    }
                    SettingKind::Group { children, .. } => {
                        settings
                            .set_field::<bool>(
                                &descriptor.id,
                                params
                                    .get(ParamID::Param(descriptor.id.ae_id()))?
                                    .as_checkbox()?
                                    .value(),
                            )
                            .map_err(|_| Error::BadCallbackParameter)?;

                        apply_settings_list(children, params, settings)?;
                    }
                }
            }

            Ok(())
        }

        apply_settings_list(&self.settings.setting_descriptors, params, &mut settings)?;

        Ok(settings)
    }
}
