use std::sync::{OnceLock, RwLock};

use gstreamer::glib;
use gstreamer::prelude::{GstParamSpecBuilderExt, ParamSpecBuilderExt, ToValue};
use gstreamer_base::prelude::BaseTransformExt;
use gstreamer_video::subclass::prelude::*;
use gstreamer_video::video_frame::VideoBufferExt;
use gstreamer_video::{
    VideoBufferFlags, VideoFieldOrder, VideoFormat, VideoFrameExt, VideoInterlaceMode,
};

use ntsc_rs::yiq_fielding::{Bgrx, Rgbx, Xbgr, Xrgb};
use ntsc_rs::{NtscEffect, settings::UseField};

use super::process_gst_frame::process_gst_frame;

#[derive(Clone, glib::Boxed, Default)]
#[boxed_type(name = "NtscFilterSettings")]
pub struct NtscFilterSettings(pub NtscEffect);

#[derive(Default)]
pub struct NtscFilter {
    info: RwLock<Option<gstreamer_video::VideoInfo>>,
    settings: RwLock<NtscFilterSettings>,
    force_interlaced_output: RwLock<bool>,
}

impl NtscFilter {
    fn output_field_order(&self) -> Option<VideoFieldOrder> {
        if !*self.force_interlaced_output.read().unwrap() {
            return None;
        }

        match self.settings.read().unwrap().0.use_field {
            UseField::InterleavedUpper => Some(VideoFieldOrder::TopFieldFirst),
            UseField::InterleavedLower => Some(VideoFieldOrder::BottomFieldFirst),
            _ => None,
        }
    }
}

#[glib::object_subclass]
impl ObjectSubclass for NtscFilter {
    const NAME: &'static str = "ntscrs";
    type Type = super::elements::NtscFilter;
    type ParentType = gstreamer_video::VideoFilter;
}

impl ObjectImpl for NtscFilter {
    fn properties() -> &'static [glib::ParamSpec] {
        static PROPERTIES: OnceLock<Vec<glib::ParamSpec>> = OnceLock::new();

        PROPERTIES.get_or_init(|| {
            vec![
                glib::ParamSpecBoxed::builder::<NtscFilterSettings>("settings")
                    .nick("Settings")
                    .blurb("ntsc-rs settings block")
                    .mutable_playing()
                    .controllable()
                    .build(),
                glib::ParamSpecBoolean::builder("force-interlaced-output")
                    .nick("Force interlaced output")
                    .blurb(
                        "Mark progressive input as 2:2 interlaced output, using the first field configured in the effect settings",
                    )
                    .mutable_ready()
                    .build(),
            ]
        })
    }

    fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
        match pspec.name() {
            "settings" => {
                let old_field_order = self.output_field_order();
                *self.settings.write().unwrap() = value.get().unwrap();
                if self.output_field_order() != old_field_order {
                    self.obj().reconfigure_src();
                }
            }
            "force-interlaced-output" => {
                *self.force_interlaced_output.write().unwrap() = value.get().unwrap();
            }
            _ => panic!("Incorrect param spec name {}", pspec.name()),
        }
    }

    fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
        match pspec.name() {
            "settings" => self.settings.read().unwrap().to_value(),
            "force-interlaced-output" => self.force_interlaced_output.read().unwrap().to_value(),
            _ => panic!("Incorrect param spec name {}", pspec.name()),
        }
    }
}

impl GstObjectImpl for NtscFilter {}

impl ElementImpl for NtscFilter {
    fn metadata() -> Option<&'static gstreamer::subclass::ElementMetadata> {
        static PROPERTIES: OnceLock<gstreamer::subclass::ElementMetadata> = OnceLock::new();
        Some(PROPERTIES.get_or_init(|| {
            gstreamer::subclass::ElementMetadata::new(
                "NTSC-rs Filter",
                "Filter/Effect/Converter/Video",
                "Applies an NTSC/VHS effect to video",
                "valadaptive",
            )
        }))
    }

    fn pad_templates() -> &'static [gstreamer::PadTemplate] {
        static PAD_TEMPLATES: OnceLock<Vec<gstreamer::PadTemplate>> = OnceLock::new();
        PAD_TEMPLATES.get_or_init(|| {
            let caps = gstreamer_video::VideoCapsBuilder::new()
                .format_list([
                    VideoFormat::Rgbx,
                    VideoFormat::Rgba,
                    VideoFormat::Bgrx,
                    VideoFormat::Bgra,
                    VideoFormat::Xrgb,
                    VideoFormat::Xbgr,
                    VideoFormat::Argb64,
                ])
                .build();

            let src_pad_template = gstreamer::PadTemplate::builder(
                "src",
                gstreamer::PadDirection::Src,
                gstreamer::PadPresence::Always,
                &caps,
            )
            .build()
            .unwrap();

            let sink_pad_template = gstreamer::PadTemplate::builder(
                "sink",
                gstreamer::PadDirection::Sink,
                gstreamer::PadPresence::Always,
                &caps,
            )
            .build()
            .unwrap();

            vec![src_pad_template, sink_pad_template]
        })
    }
}

impl BaseTransformImpl for NtscFilter {
    const MODE: gstreamer_base::subclass::BaseTransformMode =
        gstreamer_base::subclass::BaseTransformMode::NeverInPlace;
    const PASSTHROUGH_ON_SAME_CAPS: bool = false;
    const TRANSFORM_IP_ON_PASSTHROUGH: bool = false;

    // GStreamer's 2:2 interlace mode assigns both fields of each progressive input frame to one interlaced output
    // frame. The pixels themselves and framerate are unchanged; only the caps and field-order flags (metadata) differ.
    fn transform_caps(
        &self,
        direction: gstreamer::PadDirection,
        caps: &gstreamer::Caps,
        filter: Option<&gstreamer::Caps>,
    ) -> Option<gstreamer::Caps> {
        let Some(field_order) = self.output_field_order() else {
            return self.parent_transform_caps(direction, caps, filter);
        };

        let mut other_caps = caps.clone();
        match direction {
            gstreamer::PadDirection::Sink => {
                for structure in other_caps.make_mut().iter_mut() {
                    if structure
                        .get::<&str>("interlace-mode")
                        .is_ok_and(|mode| mode != "progressive")
                    {
                        // If the input is already interlaced, we ignore `use_field` and use the input's interlace
                        // order.
                        continue;
                    }

                    structure.set("interlace-mode", "interleaved");
                    structure.set("field-order", field_order.to_str());
                }
            }
            gstreamer::PadDirection::Src => {
                // Interleaved input can pass through with its original field order. Progressive input is also allowed.
                let mut progressive_caps = caps.clone();
                for structure in progressive_caps.make_mut().iter_mut() {
                    structure.set("interlace-mode", "progressive");
                    structure.remove_field("field-order");
                }
                other_caps.make_mut().append(progressive_caps);
            }
            gstreamer::PadDirection::Unknown => return None,
        }

        Some(match filter {
            Some(filter) => {
                filter.intersect_with_mode(&other_caps, gstreamer::CapsIntersectMode::First)
            }
            None => other_caps,
        })
    }

    fn copy_metadata(
        &self,
        inbuf: &gstreamer::BufferRef,
        outbuf: &mut gstreamer::BufferRef,
    ) -> Result<(), gstreamer::LoggableError> {
        self.parent_copy_metadata(inbuf, outbuf)?;

        let input_is_progressive = self
            .info
            .read()
            .unwrap()
            .as_ref()
            .is_some_and(|info| info.interlace_mode() == VideoInterlaceMode::Progressive);
        if !input_is_progressive {
            // If the input is already interlaced, we ignore `use_field` and use the input's interlace order.
            return Ok(());
        }
        let Some(field_order) = self.output_field_order() else {
            return Ok(());
        };

        outbuf.unset_video_flags(
            VideoBufferFlags::INTERLACED
                | VideoBufferFlags::TFF
                | VideoBufferFlags::RFF
                | VideoBufferFlags::ONEFIELD,
        );
        if field_order == VideoFieldOrder::TopFieldFirst {
            outbuf.set_video_flags(VideoBufferFlags::TFF);
        }

        Ok(())
    }
}

impl VideoFilterImpl for NtscFilter {
    fn set_info(
        &self,
        incaps: &gstreamer::Caps,
        in_info: &gstreamer_video::VideoInfo,
        outcaps: &gstreamer::Caps,
        out_info: &gstreamer_video::VideoInfo,
    ) -> Result<(), gstreamer::LoggableError> {
        let mut info = self.info.write().unwrap();
        *info = Some(in_info.clone());
        self.parent_set_info(incaps, in_info, outcaps, out_info)
    }

    fn transform_frame(
        &self,
        in_frame: &gstreamer_video::VideoFrameRef<&gstreamer::BufferRef>,
        out_frame: &mut gstreamer_video::VideoFrameRef<&mut gstreamer::BufferRef>,
    ) -> Result<gstreamer::FlowSuccess, gstreamer::FlowError> {
        let settings = self
            .settings
            .read()
            .or(Err(gstreamer::FlowError::Error))?
            .clone()
            .0;

        let out_stride = out_frame.plane_stride()[0] as usize;
        let out_format = out_frame.format();
        let out_data = out_frame
            .plane_data_mut(0)
            .or(Err(gstreamer::FlowError::Error))?;

        match out_format {
            VideoFormat::Rgbx | VideoFormat::Rgba => {
                process_gst_frame::<Rgbx, u8>(in_frame, out_data, out_stride, None, &settings)?;
            }
            VideoFormat::Bgrx | VideoFormat::Bgra => {
                process_gst_frame::<Bgrx, u8>(in_frame, out_data, out_stride, None, &settings)?;
            }
            VideoFormat::Xrgb | VideoFormat::Argb => {
                process_gst_frame::<Xrgb, u8>(in_frame, out_data, out_stride, None, &settings)?;
            }
            VideoFormat::Xbgr | VideoFormat::Abgr => {
                process_gst_frame::<Xbgr, u8>(in_frame, out_data, out_stride, None, &settings)?;
            }
            VideoFormat::Argb64 => {
                let data_16 = unsafe { out_data.align_to_mut::<u16>() }.1;
                process_gst_frame::<Xrgb, u16>(in_frame, data_16, out_stride, None, &settings)?;
            }
            _ => Err(gstreamer::FlowError::NotSupported)?,
        };

        Ok(gstreamer::FlowSuccess::Ok)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use gstreamer::prelude::*;
    use gstreamer_video::video_frame::VideoBufferExt;

    use super::*;

    fn run_filter(
        force_interlaced_output: bool,
        use_field: UseField,
        input_field_order: Option<VideoFieldOrder>,
    ) -> (gstreamer::Caps, VideoBufferFlags) {
        gstreamer::init().unwrap();

        let mut effect = NtscEffect::default();
        effect.use_field = use_field;

        let pipeline = gstreamer::Pipeline::default();
        let source = gstreamer::ElementFactory::make("videotestsrc")
            .property("num-buffers", 1i32)
            .build()
            .unwrap();
        let mut source_caps = gstreamer_video::VideoCapsBuilder::new()
            .format(VideoFormat::Rgbx)
            .width(16)
            .height(16)
            .framerate(gstreamer::Fraction::new(25, 1))
            .build();
        let structure = source_caps.make_mut().structure_mut(0).unwrap();
        if let Some(field_order) = input_field_order {
            structure.set("interlace-mode", "interleaved");
            structure.set("field-order", field_order.to_str());
        } else {
            structure.set("interlace-mode", "progressive");
        }
        let input_caps = gstreamer::ElementFactory::make("capsfilter")
            .property("caps", source_caps)
            .build()
            .unwrap();
        let filter: super::super::elements::NtscFilter = glib::Object::builder()
            .property("settings", NtscFilterSettings(effect))
            .property("force-interlaced-output", force_interlaced_output)
            .build();
        let sink = gstreamer::ElementFactory::make("fakesink")
            .property("sync", false)
            .property("async", false)
            .build()
            .unwrap();

        pipeline
            .add_many([&source, &input_caps, filter.upcast_ref(), &sink])
            .unwrap();
        gstreamer::Element::link_many([&source, &input_caps, filter.upcast_ref(), &sink]).unwrap();

        let flags = Arc::new(Mutex::new(None));
        let flags_clone = Arc::clone(&flags);
        let sinkpad = sink.static_pad("sink").unwrap();
        sinkpad.add_probe(gstreamer::PadProbeType::BUFFER, move |_, info| {
            if let Some(gstreamer::PadProbeData::Buffer(buffer)) = &info.data {
                *flags_clone.lock().unwrap() = Some(buffer.video_flags());
            }
            gstreamer::PadProbeReturn::Ok
        });

        pipeline.set_state(gstreamer::State::Playing).unwrap();
        let message = pipeline.bus().unwrap().timed_pop_filtered(
            gstreamer::ClockTime::from_seconds(5),
            &[gstreamer::MessageType::Eos, gstreamer::MessageType::Error],
        );
        let message = message.expect("pipeline did not finish");
        if let gstreamer::MessageView::Error(error) = message.view() {
            panic!("pipeline failed: {} ({:?})", error.error(), error.debug());
        }

        let caps = sinkpad.current_caps().expect("sink did not receive caps");
        let flags = flags
            .lock()
            .unwrap()
            .expect("sink did not receive a buffer");
        pipeline.set_state(gstreamer::State::Null).unwrap();

        (caps, flags)
    }

    #[test]
    fn preserves_progressive_output_by_default() {
        let (caps, flags) = run_filter(false, UseField::InterleavedUpper, None);
        let structure = caps.structure(0).unwrap();
        assert_eq!(structure.get::<&str>("interlace-mode"), Ok("progressive"));
        assert!(!structure.has_field("field-order"));
        assert!(!flags.contains(VideoBufferFlags::TFF));
    }

    #[test]
    fn forces_top_field_first_output_from_use_field() {
        let (caps, flags) = run_filter(true, UseField::InterleavedUpper, None);
        let structure = caps.structure(0).unwrap();
        assert_eq!(structure.get::<&str>("interlace-mode"), Ok("interleaved"));
        assert_eq!(structure.get::<&str>("field-order"), Ok("top-field-first"));
        assert_eq!(
            structure.get::<gstreamer::Fraction>("framerate"),
            Ok(25.into())
        );
        assert!(flags.contains(VideoBufferFlags::TFF));
        assert!(!flags.intersects(
            VideoBufferFlags::INTERLACED | VideoBufferFlags::RFF | VideoBufferFlags::ONEFIELD,
        ));
    }

    #[test]
    fn forces_bottom_field_first_output_from_use_field() {
        let (caps, flags) = run_filter(true, UseField::InterleavedLower, None);
        let structure = caps.structure(0).unwrap();
        assert_eq!(structure.get::<&str>("interlace-mode"), Ok("interleaved"));
        assert_eq!(
            structure.get::<&str>("field-order"),
            Ok("bottom-field-first")
        );
        assert!(!flags.intersects(
            VideoBufferFlags::INTERLACED
                | VideoBufferFlags::TFF
                | VideoBufferFlags::RFF
                | VideoBufferFlags::ONEFIELD,
        ));
    }

    #[test]
    fn preserves_interlaced_input_order_instead_of_use_field() {
        let (caps, flags) = run_filter(
            true,
            UseField::InterleavedUpper,
            Some(VideoFieldOrder::BottomFieldFirst),
        );
        let structure = caps.structure(0).unwrap();
        assert_eq!(structure.get::<&str>("interlace-mode"), Ok("interleaved"));
        assert_eq!(
            structure.get::<&str>("field-order"),
            Ok("bottom-field-first")
        );
        assert!(!flags.contains(VideoBufferFlags::TFF));
    }
}
