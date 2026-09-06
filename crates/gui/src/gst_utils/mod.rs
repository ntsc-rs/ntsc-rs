pub mod clock_format;
pub mod egui_sink;
pub mod gstreamer_error;
pub mod image_freeze;
pub mod init;
pub mod multi_file_path;
pub mod ntsc_pipeline;
pub mod ntscrs_filter;
pub mod process_gst_frame;
pub mod video_pad_filter;

pub mod elements {
    use super::{egui_sink, image_freeze, ntscrs_filter, video_pad_filter};
    use gstreamer::glib;
    glib::wrapper! {
        pub struct EguiSink(ObjectSubclass<egui_sink::EguiSink>) @extends gstreamer_video::VideoSink, gstreamer_base::BaseSink, gstreamer::Element, gstreamer::Object;
    }

    glib::wrapper! {
        pub struct ImageFreeze(ObjectSubclass<image_freeze::ImageFreeze>) @extends gstreamer::Element, gstreamer::Object;
    }

    glib::wrapper! {
        pub struct NtscFilter(ObjectSubclass<ntscrs_filter::NtscFilter>) @extends gstreamer_video::VideoFilter, gstreamer_base::BaseTransform, gstreamer::Element, gstreamer::Object;
    }

    glib::wrapper! {
        pub struct VideoPadFilter(ObjectSubclass<video_pad_filter::VideoPadFilter>) @extends gstreamer_video::VideoFilter, gstreamer_base::BaseTransform, gstreamer::Element, gstreamer::Object;
    }
}

pub fn scale_from_caps(caps: &gstreamer::Caps, scanlines: usize) -> Option<(i32, i32)> {
    let caps_structure = caps.structure(0)?;
    let src_width = caps_structure.get::<i32>("width").ok()?;
    let src_height = caps_structure.get::<i32>("height").ok()?;

    let scale_factor = scanlines as f32 / src_height as f32;
    let dst_width = (src_width as f32 * scale_factor).round() as i32;

    Some((dst_width, scanlines as i32))
}
