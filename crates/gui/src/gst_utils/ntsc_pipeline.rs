use super::{gstreamer_error::GstreamerError, scale_from_caps};
use gstreamer::{
    ClockTime, Pipeline, element_error, element_warning,
    format::SpecificFormattedValueIntrinsic,
    glib::{self, BoolError},
    prelude::*,
    query::Duration,
};
use gstreamer_pbutils::MissingPluginMessage;
use gstreamer_video::{VideoCapsBuilder, VideoInterlaceMode};
use log::debug;
use serde::{Deserialize, Serialize};
use std::{
    error::Error,
    fmt::Display,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32},
    },
};

#[derive(Clone, Debug, glib::Boxed)]
#[boxed_type(name = "ErrorValue")]
#[allow(dead_code)]
struct ErrorValue(Arc<Mutex<Option<GstreamerError>>>);

#[derive(Debug, Clone)]
pub enum PipelineError {
    GlibError(glib::Error),
    MissingPluginError {
        description: Option<glib::GString>,
        source: glib::Error,
    },
    NoVideoError,
}

impl Display for PipelineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GlibError(e) => e.fmt(f),
            Self::MissingPluginError { description, .. } => match description {
                Some(description) => write!(f, "Missing a GStreamer plugin: {description}"),
                None => write!(f, "Missing a GStreamer plugin"),
            },
            Self::NoVideoError => f.write_str("Pipeline source has no video"),
        }
    }
}

impl Error for PipelineError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::GlibError(e) => Some(e),
            Self::MissingPluginError { source, .. } => Some(source),
            Self::NoVideoError => None,
        }
    }
}

impl From<glib::Error> for PipelineError {
    fn from(value: glib::Error) -> Self {
        Self::GlibError(value)
    }
}

#[derive(Debug, Clone)]
pub struct VideoElemMetadata {
    pub is_still_image: bool,
    pub interlace_mode: Option<VideoInterlaceMode>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct VideoScale {
    pub scanlines: usize,
    pub filter: VideoScaleFilter,
}

impl Default for VideoScale {
    fn default() -> Self {
        Self {
            scanlines: 480,
            filter: Default::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum VideoScaleFilter {
    Nearest,
    #[default]
    Bilinear,
    Bicubic,
}

impl VideoScaleFilter {
    pub fn as_string_value(&self) -> &'static str {
        match self {
            Self::Nearest => "nearest-neighbour",
            Self::Bilinear => "bilinear",
            Self::Bicubic => "catrom",
        }
    }

    pub fn label_and_tooltip(&self) -> (&'static str, &'static str) {
        match self {
            Self::Nearest => (
                "Nearest",
                "Nearest-neighbor (pixelated) scaling. Note that this is still slower than \
                 Bilinear",
            ),
            Self::Bilinear => ("Bilinear", "Lower-quality but faster scaling filter"),
            Self::Bicubic => (
                "Bicubic",
                "Sharper scaling filter; ~20% slower than Bilinear",
            ),
        }
    }

    pub fn values() -> &'static [Self] {
        &[Self::Nearest, Self::Bilinear, Self::Bicubic]
    }
}

#[derive(Debug, Clone)]
pub struct NtscPipeline {
    pub inner: Pipeline,
    /// Incremented and set as a custom property on the caps filter to force renegotiation.
    caps_generation: Arc<AtomicU32>,
}

impl NtscPipeline {
    pub fn try_new<
        AudioElemCallback: FnOnce(&gstreamer::Pipeline) -> Result<Option<gstreamer::Element>, GstreamerError>
            + Send
            + Sync
            + 'static,
        VideoElemCallback: FnOnce(
                &gstreamer::Pipeline,
                VideoElemMetadata,
            ) -> Result<gstreamer::Element, GstreamerError>
            + Send
            + Sync
            + 'static,
        BusHandler: Fn(&gstreamer::Bus, &gstreamer::Message) -> gstreamer::BusSyncReply + Send + Sync + 'static,
        PipelineCallback: FnOnce(Result<gstreamer::Pipeline, PipelineError>) + Send + Sync + 'static,
    >(
        src_pad: gstreamer::Element,
        audio_sink: AudioElemCallback,
        video_sink: VideoElemCallback,
        bus_handler: BusHandler,
        still_image_duration: Option<gstreamer::ClockTime>,
        initial_scale: Option<VideoScale>,
        initial_still_image_framerate: gstreamer::Fraction,
        callback: Option<PipelineCallback>,
    ) -> Result<Self, GstreamerError> {
        let pipeline = gstreamer::Pipeline::default();
        let decodebin = gstreamer::ElementFactory::make("decodebin").build()?;
        pipeline.add_many([&src_pad, &decodebin])?;
        gstreamer::Element::link_many([&src_pad, &decodebin])?;

        let has_audio = Mutex::new(false);
        let has_video = Arc::new(Mutex::new(false));
        let has_video_for_bus_handler = Arc::clone(&has_video);

        let handler_id: Arc<Mutex<Option<glib::SignalHandlerId>>> = Arc::new(Mutex::new(None));
        let handler_id_for_handler = Arc::clone(&handler_id);

        let audio_sink = Mutex::new(Some(audio_sink));
        let video_sink = Mutex::new(Some(video_sink));

        let pipeline_weak = gstreamer::prelude::ObjectExt::downgrade(&pipeline);
        let pad_added_handler = decodebin.connect_pad_added(move |dbin, src_pad| {
            let pipeline = &pipeline_weak;
            let handler_id = &handler_id_for_handler;
            // Try to detect whether the raw stream decodebin provided us with
            // just now is either audio or video (or none of both, e.g. subtitles).
            let (is_audio, is_video) = {
                let media_type = src_pad.current_caps().and_then(|caps| {
                    debug!("{:?}", caps);
                    caps.structure(0).map(|s| {
                        let name = s.name();
                        (name.starts_with("audio/"), name.starts_with("video/"))
                    })
                });

                match media_type {
                    None => {
                        element_warning!(
                            dbin,
                            gstreamer::CoreError::Negotiation,
                            ("Failed to get media type from pad {}", src_pad.name())
                        );

                        return;
                    }
                    Some(media_type) => media_type,
                }
            };

            let insert_sink = |is_audio, is_video| -> Result<(), GstreamerError> {
                let mut has_audio = has_audio.lock().unwrap();
                let mut has_video = has_video.lock().unwrap();
                if is_audio && !*has_audio {
                    debug!("connected audio");

                    if let Some(pipeline) = pipeline.upgrade() {
                        let audio_sink = audio_sink.lock().unwrap().take();
                        if let Some(sink) = audio_sink.map(|sink| sink(&pipeline))
                            && let Some(sink) = sink?
                        {
                            let audio_queue = gstreamer::ElementFactory::make("queue").build()?;
                            let audio_convert =
                                gstreamer::ElementFactory::make("audioconvert").build()?;
                            let audio_resample =
                                gstreamer::ElementFactory::make("audioresample").build()?;
                            let audio_volume = gstreamer::ElementFactory::make("volume")
                                .name("audio_volume")
                                .build()?;

                            let audio_elements =
                                &[&audio_queue, &audio_convert, &audio_resample, &audio_volume];
                            pipeline.add_many(audio_elements)?;
                            gstreamer::Element::link_many(audio_elements)?;

                            audio_volume.link(&sink)?;
                            sink.sync_state_with_parent()?;

                            for e in audio_elements {
                                e.sync_state_with_parent()?
                            }

                            // Get the queue element's sink pad and link the decodebin's newly created
                            // src pad for the audio stream to it.
                            let sink_pad = audio_queue
                                .static_pad("sink")
                                .expect("queue has no sinkpad");
                            src_pad.link(&sink_pad)?;
                        }

                        *has_audio = true;
                    }

                    // has_audio
                } else if is_video && !*has_video {
                    debug!("connected video");

                    if let Some(pipeline) = pipeline.upgrade() {
                        let video_sink = video_sink.lock().unwrap().take();
                        if let Some(video_sink) = video_sink {
                            let video_queue = gstreamer::ElementFactory::make("queue")
                                .name("video_queue")
                                .build()?;
                            let video_convert =
                                gstreamer::ElementFactory::make("videoconvert").build()?;
                            // TODO: Figure out how to make the scale take video orientation into account. Currently, videos
                            // that get reoriented keep their old scales.
                            /*let video_flip = gstreamer::ElementFactory::make("videoflip")
                            .name("video_flip")
                            .property(
                                "video-direction",
                                gstreamer_video::VideoOrientationMethod::Auto,
                            )
                            .build()?;*/
                            let video_rate = gstreamer::ElementFactory::make("videorate")
                                .name("video_rate")
                                .build()?;

                            let video_scale = gstreamer::ElementFactory::make("videoscale")
                                .name("video_scale")
                                .property_from_str(
                                    "method",
                                    initial_scale
                                        .map(|scale| scale.filter)
                                        .unwrap_or_default()
                                        .as_string_value(),
                                )
                                .build()?;

                            let caps_filter = gstreamer::ElementFactory::make("capsfilter")
                                .name("caps_filter")
                                .build()?;
                            let framerate_caps_filter =
                                gstreamer::ElementFactory::make("capsfilter")
                                    .name("framerate_caps_filter")
                                    .build()?;

                            let video_elements = &[
                                &video_queue,
                                //&video_flip,
                                &video_convert,
                                &video_rate,
                                &video_scale,
                                &caps_filter,
                                &framerate_caps_filter,
                            ];
                            pipeline.add_many(video_elements)?;
                            gstreamer::Element::link_many(video_elements)?;

                            let caps = src_pad.current_caps();
                            let caps = caps.as_ref();
                            let structure = caps.and_then(|caps| caps.structure(0));

                            let framerate = structure.and_then(|structure| {
                                structure.get::<gstreamer::Fraction>("framerate").ok()
                            });

                            let interlace_mode = structure.and_then(|structure| {
                                Some(VideoInterlaceMode::from_string(
                                    structure.get("interlace-mode").ok()?,
                                ))
                            });

                            let mut q = Duration::new(gstreamer::Format::Time);
                            let _ = src_pad.query(q.query_mut());
                            let src_duration = q.result();
                            let has_duration = src_duration.is_some();

                            let has_zero_framerate = match framerate {
                                Some(framerate) => framerate.numer() == 0,
                                None => false,
                            };

                            // TODO: Videos with a variable framerate have a framerate of 0, so we also check if the input
                            // has a defined duration (see https://github.com/valadaptive/ntsc-rs/issues/8). Since we do so,
                            // is it still necessary to have *both* checks? Are there some videos that are not still images
                            // but still have no duration?
                            let is_still_image = has_zero_framerate && !has_duration;

                            let video_sink = video_sink(
                                &pipeline,
                                VideoElemMetadata {
                                    is_still_image,
                                    interlace_mode,
                                },
                            )?;
                            video_elements.last().unwrap().link(&video_sink)?;

                            for e in video_elements {
                                e.sync_state_with_parent()?;
                            }
                            video_sink.sync_state_with_parent()?;

                            // Get the queue element's sink pad and link the decodebin's newly created
                            // src pad for the video stream to it.
                            let sink_pad = video_queue
                                .static_pad("sink")
                                .expect("queue has no sinkpad");

                            if let (Some(caps), Some(initial_scale)) = (caps, initial_scale)
                                && let Some((width, height)) =
                                    scale_from_caps(caps, initial_scale.scanlines)
                            {
                                caps_filter.set_property(
                                    "caps",
                                    gstreamer_video::VideoCapsBuilder::default()
                                        .width(width)
                                        .height(height)
                                        .build(),
                                );
                            }

                            if is_still_image {
                                let image_freeze = gstreamer::ElementFactory::make("imagefreeze")
                                    .name("still_image_freeze")
                                    .build()?;
                                let video_caps = gstreamer_video::VideoCapsBuilder::new()
                                    .framerate(initial_still_image_framerate)
                                    .build();
                                framerate_caps_filter.set_property("caps", video_caps);

                                pipeline.add(&image_freeze)?;
                                src_pad.link(&image_freeze.static_pad("sink").unwrap())?;
                                gstreamer::Element::link_many([&image_freeze, &video_queue])?;
                                image_freeze.sync_state_with_parent()?;

                                // We cannot move this functionality into create_render_job. If we do this seek outside of
                                // this callback, the output video will be truncated. No idea why.
                                if let Some(duration) = still_image_duration {
                                    image_freeze.seek(
                                        1.0,
                                        gstreamer::SeekFlags::FLUSH
                                            | gstreamer::SeekFlags::ACCURATE,
                                        gstreamer::SeekType::Set,
                                        gstreamer::ClockTime::ZERO,
                                        gstreamer::SeekType::Set,
                                        duration,
                                    )?;
                                }
                            } else {
                                src_pad.link(&sink_pad)?;
                            }
                        }
                    }

                    *has_video = true;
                }

                // We have both streams. No need to search for more.
                if *has_audio && *has_video {
                    let mut id = handler_id.lock().unwrap();
                    let id = id.take();
                    if let Some(id) = id {
                        dbin.disconnect(id);
                    }
                }

                Ok(())
            };

            // When adding and linking new elements in a callback fails, error information is often sparse.
            // GStreamer's built-in debugging can be hard to link back to the exact position within the code
            // that failed. Since callbacks are called from random threads within the pipeline, it can get hard
            // to get good error information. The macros used in the following can solve that. With the use
            // of those, one can send arbitrary rust types (using the pipeline's bus) into the mainloop.
            // What we send here is unpacked down below, in the iteration-code over sent bus-messages.
            if let Err(err) = insert_sink(is_audio, is_video) {
                debug!("got error: {:?}", err);
                element_error!(
                    dbin,
                    gstreamer::LibraryError::Failed,
                    ("Failed to insert sink"),
                    details: gstreamer::Structure::builder("error-details")
                                .field("error",
                                    ErrorValue(Arc::new(Mutex::new(Some(err)))))
                                .build()
                );
            }
        });

        handler_id.lock().unwrap().replace(pad_added_handler);

        let bus = pipeline
            .bus()
            .expect("Pipeline without bus. Shouldn't happen!");

        let finished_loading = AtomicBool::new(false);
        let missing_plugin_message = Mutex::new(None);
        let handler_callback = Mutex::new(callback);
        bus.set_sync_handler(move |bus, msg| {
            if !finished_loading.load(std::sync::atomic::Ordering::SeqCst) {
                if let Ok(m) = MissingPluginMessage::parse(msg) {
                    *missing_plugin_message.lock().unwrap() = Some(m.description());
                }
                match msg.view() {
                    gstreamer::MessageView::AsyncDone(a) => {
                        if let Some(pipeline) = a
                            .src()
                            .and_then(|a| a.downcast_ref::<gstreamer::Pipeline>())
                        {
                            debug!("pipeline state change done");

                            let mut id = handler_id.lock().unwrap();
                            let id = id.take();
                            if let Some(id) = id {
                                decodebin.disconnect(id);
                            }
                            if let Some(callback) = handler_callback.lock().unwrap().take() {
                                if *has_video_for_bus_handler.lock().unwrap() {
                                    callback(Ok(pipeline.clone()));
                                } else {
                                    callback(Err(PipelineError::NoVideoError));
                                }
                            }
                            finished_loading.store(true, std::sync::atomic::Ordering::SeqCst);
                        }
                    }
                    gstreamer::MessageView::Error(e) => {
                        let source_err = e.error();
                        let err = if let Some(gstreamer::CoreError::MissingPlugin) =
                            source_err.kind::<gstreamer::CoreError>()
                        {
                            PipelineError::MissingPluginError {
                                description: missing_plugin_message.lock().unwrap().take(),
                                source: source_err,
                            }
                        } else {
                            PipelineError::GlibError(source_err)
                        };
                        if let Some(callback) = handler_callback.lock().unwrap().take() {
                            callback(Err(err));
                        }
                        finished_loading.store(true, std::sync::atomic::Ordering::SeqCst);
                    }
                    _ => {}
                }
            }

            bus_handler(bus, msg)
        });

        Ok(Self {
            inner: pipeline,
            caps_generation: Arc::new(AtomicU32::new(0)),
        })
    }

    pub fn set_still_image_framerate(
        &self,
        framerate: gstreamer::Fraction,
    ) -> Result<Option<gstreamer::Fraction>, GstreamerError> {
        let pipeline = &self.inner;
        let Some(caps_filter) = pipeline.by_name("framerate_caps_filter") else {
            return Ok(None);
        };

        caps_filter.set_property(
            "caps",
            VideoCapsBuilder::default().framerate(framerate).build(),
        );
        // This seek is necessary to prevent caps negotiation from failing due to race conditions, for some reason.
        // It seems like in some cases, there would be "tearing" in the caps between different elements, where some
        // elements' caps would use the old framerate and some would use the new framerate. This would cause caps
        // negotiation to fail, even though the caps filter sends a "reconfigure" event. This in turn woulc make the
        // entire pipeline error out.
        if let Some(seek_pos) = pipeline.query_position::<ClockTime>() {
            pipeline.seek_simple(
                gstreamer::SeekFlags::FLUSH | gstreamer::SeekFlags::ACCURATE,
                seek_pos,
            )?;
            Ok(Some(framerate))
        } else {
            Ok(None)
        }
    }

    pub fn set_volume(&self, volume: f64, mute: bool) {
        let Some(audio_volume) = self.inner.by_name("audio_volume") else {
            return;
        };

        audio_volume.set_property("volume", volume);
        audio_volume.set_property("mute", mute);
    }

    pub fn rescale_video(
        &self,
        seek_pos: ClockTime,
        scale: Option<VideoScale>,
    ) -> Result<(), GstreamerError> {
        let pipeline = &self.inner;
        let caps_filter = pipeline.by_name("caps_filter").unwrap();

        if let Some(scale) = scale {
            let Some(scale_elem) = pipeline.by_name("video_scale") else {
                return Ok(());
            };
            let Some(scale_caps) = scale_elem
                .static_pad("sink")
                .and_then(|pad| pad.current_caps())
            else {
                return Ok(());
            };

            let caps_generation = self
                .caps_generation
                .load(std::sync::atomic::Ordering::Acquire)
                .wrapping_add(1);
            self.caps_generation
                .store(caps_generation, std::sync::atomic::Ordering::Release);
            if let Some((dst_width, dst_height)) = scale_from_caps(&scale_caps, scale.scanlines) {
                caps_filter.set_property(
                    "caps",
                    gstreamer_video::VideoCapsBuilder::default()
                        .width(dst_width)
                        .height(dst_height)
                        // The videoscale element's properties do not take effect until caps are renegotiated, which
                        // doesn't happen if the old and new caps are equal. Increment the custom "generation" field on
                        // the filtered caps to force renegotiation to actually occur.
                        .field("generation", caps_generation)
                        .build(),
                );
                scale_elem.set_property_from_str("method", scale.filter.as_string_value());
            }
        } else {
            caps_filter.set_property("caps", gstreamer_video::VideoCapsBuilder::default().build());
        }

        pipeline.seek_simple(
            gstreamer::SeekFlags::FLUSH | gstreamer::SeekFlags::ACCURATE,
            pipeline.query_position::<ClockTime>().unwrap_or(seek_pos),
        )?;

        Ok(())
    }

    pub fn set_state(
        &self,
        state: gstreamer::State,
    ) -> Result<gstreamer::StateChangeSuccess, gstreamer::StateChangeError> {
        self.inner.set_state(state)
    }

    pub fn query_position<T: SpecificFormattedValueIntrinsic>(&self) -> Option<T> {
        self.inner.query_position::<T>()
    }

    pub fn query_duration<T: SpecificFormattedValueIntrinsic>(&self) -> Option<T> {
        self.inner.query_duration::<T>()
    }

    pub fn seek_simple(
        &self,
        flags: gstreamer::SeekFlags,
        seek_pos: impl FormattedValue,
    ) -> Result<(), BoolError> {
        self.inner.seek_simple(flags, seek_pos)
    }

    pub fn current_state(&self) -> gstreamer::State {
        self.inner.current_state()
    }
}
