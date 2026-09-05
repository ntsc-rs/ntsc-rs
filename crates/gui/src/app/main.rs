use std::{
    cell::RefCell,
    error::Error,
    ffi::OsStr,
    fs::File,
    io::Read,
    ops::RangeInclusive,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
};

use blocking::unblock;
use eframe::egui::{
    self, Color32, ColorImage, DroppedFileHandle, Response, TextureOptions, UiBuilder,
    util::undoer::Undoer, vec2,
};
use futures_lite::Future;
use gstreamer::{ClockTime, Fraction, glib::subclass::types::ObjectSubclassExt, prelude::*};
use gstreamer_video::VideoInterlaceMode;

use crate::{
    app::update_dialog::UpdateDialogState,
    expression_parser::eval_expression_string,
    gst_utils::{
        clock_format::{clock_time_format, clock_time_parser},
        egui_sink::{EffectPreviewSetting, EguiCtx, EguiSink, SinkTexture},
        elements,
        gstreamer_error::GstreamerError,
        init::initialize_gstreamer,
        ntsc_pipeline::{NtscPipeline, PipelineError, VideoElemMetadata, VideoScaleFilter},
        ntscrs_filter::NtscFilterSettings,
    },
    widgets::{
        render_job::{RenderJobResponse, RenderJobWidget},
        splitscreen::SplitScreen,
        timeline::Timeline,
    },
};

use ntsc_rs::settings::{
    EnumValue as SettingsEnumValue, SettingDescriptor, SettingKind, Settings, SettingsList,
    easy::{self, EasyMode},
    standard::{NtscEffect, setting_id},
};
use snafu::ResultExt;

use log::debug;

use super::{
    AppFn, NtscApp,
    app_state::{
        AudioVolume, EffectPreviewMode, EffectPreviewSettings, GstreamerInitState, LeftPanelState,
        VideoScaleState, VideoZoom,
    },
    dnd_overlay::{CtxDndExt, UiDndExt},
    error::{
        ApplicationError, CreatePresetFileSnafu, CreatePresetJSONSnafu, CreateRenderJobSnafu,
        FsSnafu, JSONParseSnafu, JSONReadSnafu, LoadVideoSnafu,
    },
    executor::AppExecutor,
    layout_helper::{LayoutHelper, PanelExt},
    pipeline_info::{PipelineInfo, PipelineMetadata, PipelineStatus},
    presets::PresetsState,
    render_job::RenderJob,
    render_settings::{
        Ffv1BitDepth, H264Settings, OutputCodec, PngSequenceSettings, PngSettings,
        RenderInterlaceMode, RenderPipelineCodec, RenderPipelineSettings, RenderSettings,
        StillImageSettings,
    },
    system_fonts::system_fallback_fonts,
};

const EXPERIMENTAL_EASY_MODE: bool = false;

fn format_percentage(n: f64, prec: RangeInclusive<usize>) -> String {
    format!("{:.*}%", prec.start().max(&2) - 2, n * 100.0)
}

/// Parse a textbox input as either a decimal or percentage, depending on whether it's greater than a certain threshold.
/// Returns a decimal.
///
/// # Arguments
/// - `input` - The text input from the user.
/// - `threshold` - The number above which the input will be treated as a percentage rather than a decimal.
fn parse_decimal_or_percentage(input: &str, threshold: f64) -> Option<f64> {
    let mut expr = eval_expression_string(input).ok()?;
    if expr >= threshold {
        // The user probably meant to input a raw percentage and not a decimal in 0..1
        expr /= 100.0;
    }
    Some(expr)
}

#[cfg(not(target_os = "macos"))]
static ICON: &[u8] = include_bytes!("../../../../assets/icon.png");

pub fn run() -> Result<(), Box<dyn Error>> {
    env_logger::init(); // Log to stderr (if you run with `RUST_LOG=debug`).

    let viewport = egui::ViewportBuilder::default().with_inner_size([1300.0, 720.0]);

    // Use the bundle icon for macOS
    #[cfg(not(target_os = "macos"))]
    let viewport = viewport.with_icon(eframe::icon_data::from_png_bytes(ICON)?);
    #[cfg(target_os = "macos")]
    let viewport = viewport.with_icon(egui::IconData::default());

    let options = eframe::NativeOptions {
        viewport: viewport
            .with_inner_size([1300.0, 720.0])
            .with_app_id(NtscApp::APP_ID),
        ..Default::default()
    };

    Ok(eframe::run_native(
        NtscApp::APP_ID,
        options,
        Box::new(|cc| {
            let ctx = cc.egui_ctx.clone();
            // Load fonts and GStreamer in separate threads. Cascade the JoinHandles.
            let fonts_thread_handle = {
                let ctx = ctx.clone();
                thread::spawn(move || {
                    debug!("Loading fonts");
                    for font in system_fallback_fonts() {
                        ctx.add_font(font);
                    }
                    debug!("Loaded fonts");
                })
            };
            let handle = thread::spawn(move || {
                // GStreamer can be slow to initialize (on the order of minutes). Do it off-thread so we can display a
                // loading screen in the meantime. Thanks for being thread-safe, unlike GTK!
                debug!("Loading GStreamer");
                let init_result = initialize_gstreamer();
                debug!("Loaded GStreamer");
                fonts_thread_handle.join().unwrap();
                init_result
            });
            let init_state = GstreamerInitState::Initializing(Some(handle));

            let settings_list = SettingsList::<NtscEffect>::new();
            let settings_list_easy = SettingsList::<EasyMode>::new();
            let (
                settings,
                easy_mode_settings,
                mut easy_mode_enabled,
                render_settings,
                scale_settings,
            ) = if let Some(storage) = cc.storage {
                // Load previous effect settings from storage
                let settings = storage
                    .get_string("effect_settings")
                    .and_then(|saved_settings| settings_list.from_json(&saved_settings).ok())
                    .unwrap_or_default();
                let easy_mode_settings = storage
                    .get_string("easy_mode_settings")
                    .and_then(|saved_settings| {
                        settings_list_easy.from_json_generic(&saved_settings).ok()
                    })
                    .unwrap_or_default();
                let easy_mode_enabled = storage
                    .get_string("easy_mode_enabled")
                    .map(|saved_enabled| saved_enabled == "true")
                    .unwrap_or_default();
                let render_settings =
                    eframe::get_value::<RenderSettings>(storage, "render_settings")
                        .unwrap_or_default();
                let scale_settings =
                    eframe::get_value::<VideoScaleState>(storage, "scale_settings")
                        .unwrap_or_default();

                (
                    settings,
                    easy_mode_settings,
                    easy_mode_enabled,
                    render_settings,
                    scale_settings,
                )
            } else {
                (
                    NtscEffect::default(),
                    EasyMode::default(),
                    true,
                    RenderSettings::default(),
                    VideoScaleState::default(),
                )
            };

            easy_mode_enabled &= EXPERIMENTAL_EASY_MODE;

            ctx.global_style_mut(|style| style.interaction.tooltip_delay = 0.5);
            Ok(Box::new(NtscApp::new(
                ctx,
                settings_list,
                settings_list_easy,
                settings,
                easy_mode_settings,
                easy_mode_enabled,
                render_settings,
                scale_settings,
                init_state,
            )))
        }),
    )?)
}

impl NtscApp {
    pub const APP_ID: &'static str = "ntsc-rs";

    fn new(
        ctx: egui::Context,
        settings_list: SettingsList<NtscEffect>,
        settings_list_easy: SettingsList<EasyMode>,
        effect_settings: NtscEffect,
        easy_mode_settings: EasyMode,
        easy_mode_enabled: bool,
        render_settings: RenderSettings,
        scale_settings: VideoScaleState,
        gstreamer_init: GstreamerInitState,
    ) -> Self {
        Self {
            gstreamer_init,
            settings_list,
            settings_list_easy,
            pipeline: None,
            undoer: Undoer::default(),
            executor: AppExecutor::new(ctx.clone()),
            video_zoom: VideoZoom {
                scale: 1.0,
                fit: true,
            },
            video_scale: scale_settings,
            audio_volume: AudioVolume::default(),
            effect_preview: EffectPreviewSettings::default(),
            left_panel_state: LeftPanelState::default(),
            easy_mode_enabled,
            effect_settings,
            easy_mode_settings,
            presets_state: PresetsState::default(),
            render_settings,
            render_jobs: Vec::new(),
            settings_json_paste: String::new(),
            last_error: RefCell::new(None),
            credits_dialog_open: false,
            third_party_licenses_dialog_open: false,
            license_dialog_open: false,
            update_dialog: UpdateDialogState::Closed,
            image_sequence_dialog_queued_render_job: None,
        }
    }

    pub fn spawn(&self, future: impl Future<Output = Option<AppFn>> + 'static + Send) {
        self.executor.spawn(future);
    }

    fn tick(&mut self) {
        loop {
            // Get the functions to be executed at the end of the completed futures.
            let app_fns = self.executor.tick();

            // If there are none, we're done. If there are, loop--executing them may spawn more futures.
            if app_fns.is_empty() {
                break;
            }

            // Execute functions outside the executor--if they call `spawn`, we don't want to recursively lock the
            // executor's mutex.
            for f in app_fns {
                let result = f(self);
                self.handle_result(result);
            }
        }
    }

    fn load_video(&mut self, ctx: &egui::Context, path: PathBuf) -> Result<(), ApplicationError> {
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(
            path.file_name()
                .map(|file_name| format!("{} — {}", Self::APP_ID, file_name.to_string_lossy()))
                .unwrap_or_else(|| Self::APP_ID.to_string()),
        ));

        self.pipeline = Some(
            self.create_preview_pipeline(ctx, path)
                .context(LoadVideoSnafu)?,
        );

        Ok(())
    }

    fn close_video(&mut self, ctx: &egui::Context) {
        self.pipeline = None;
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(Self::APP_ID.to_string()));
    }

    fn sink_preview_mode(preview_settings: &EffectPreviewSettings) -> EffectPreviewSetting {
        match preview_settings.mode {
            EffectPreviewMode::Enabled => EffectPreviewSetting::Enabled,
            EffectPreviewMode::Disabled => EffectPreviewSetting::Disabled,
            EffectPreviewMode::SplitScreen => {
                EffectPreviewSetting::SplitScreen(preview_settings.preview_rect)
            }
        }
    }

    fn create_preview_pipeline(
        &mut self,
        ctx: &egui::Context,
        path: PathBuf,
    ) -> Result<PipelineInfo, GstreamerError> {
        let src = gstreamer::ElementFactory::make("filesrc")
            .property("location", path.as_path())
            .build()?;

        let audio_sink = gstreamer::ElementFactory::make("autoaudiosink").build()?;

        let tex_sink = SinkTexture::new();
        let egui_ctx = EguiCtx(Some(ctx.clone()));
        let video_sink = gstreamer::ElementFactory::make("eguisink")
            .property("texture", tex_sink)
            .property("ctx", egui_ctx)
            .property("settings", NtscFilterSettings(self.effect_settings.clone()))
            .property(
                "preview-mode",
                Self::sink_preview_mode(&self.effect_preview),
            )
            .build()?;

        let pipeline_info_state = Arc::new(Mutex::new(PipelineStatus::Loading));
        let pipeline_info_state_for_handler = Arc::clone(&pipeline_info_state);
        let pipeline_info_state_for_callback = Arc::clone(&pipeline_info_state);
        let at_eos = Arc::new(Mutex::new(false));
        let at_eos_for_handler = Arc::clone(&at_eos);
        let ctx_for_handler = ctx.clone();
        let ctx_for_callback = ctx.clone();

        let metadata = Arc::new(Mutex::new(PipelineMetadata::default()));
        let metadata_for_audio_handler = metadata.clone();
        let metadata_for_bus_handler = metadata.clone();

        let audio_sink_for_closure = audio_sink.clone();
        let video_sink_for_closure = video_sink.clone();

        let pipeline = NtscPipeline::try_new(
            src.clone(),
            move |pipeline| {
                pipeline.add(&audio_sink_for_closure)?;
                metadata_for_audio_handler.lock().unwrap().has_audio = Some(true);
                Ok(Some(audio_sink_for_closure))
            },
            move |pipeline, VideoElemMetadata { .. }| {
                pipeline.add(&video_sink_for_closure)?;
                Ok(video_sink_for_closure)
            },
            move |bus, msg| {
                debug!("{:?}", msg);
                let at_eos = &at_eos_for_handler;
                let ctx = &ctx_for_handler;
                let pipeline_info_state = &pipeline_info_state_for_handler;
                let metadata = &metadata_for_bus_handler;

                let handle_msg = move |_bus, msg: &gstreamer::Message| -> Option<()> {
                    // Make sure we're listening to a pipeline event
                    let src = msg.src()?;

                    if let gstreamer::MessageView::Error(err_msg) = msg.view() {
                        debug!("handling error message: {:?}", msg);
                        let mut pipeline_state = pipeline_info_state.lock().unwrap();
                        if !matches!(&*pipeline_state, PipelineStatus::Error(_)) {
                            *pipeline_state = PipelineStatus::Error(err_msg.error().into());
                            ctx.request_repaint();
                        }
                    }

                    if let Some(pipeline) = src.downcast_ref::<gstreamer::Pipeline>() {
                        // We want to pause the pipeline at EOS, but setting an element's state inside the bus handler doesn't
                        // work. Instead, wait for the next egui event loop then pause.
                        if let gstreamer::MessageView::Eos(_) = msg.view() {
                            *at_eos.lock().unwrap() = true;
                            ctx.request_repaint();
                        }

                        if let gstreamer::MessageView::StateChanged(state_changed) = msg.view()
                            && state_changed.old() == gstreamer::State::Ready
                            && matches!(
                                state_changed.current(),
                                gstreamer::State::Paused | gstreamer::State::Playing
                            )
                        {
                            // Changed from READY to PAUSED/PLAYING.
                            *pipeline_info_state.lock().unwrap() = PipelineStatus::Loaded;

                            let mut metadata = metadata.lock().unwrap();

                            let is_still_image = pipeline.by_name("still_image_freeze").is_some();
                            metadata.is_still_image = Some(is_still_image);

                            let video_rate = pipeline.by_name("video_rate");
                            let caps = video_rate.and_then(|video_rate| {
                                video_rate
                                    .static_pad("src")
                                    .and_then(|pad| pad.current_caps())
                            });

                            if let Some(caps) = caps {
                                let structure = caps.structure(0);

                                metadata.framerate = structure.and_then(|structure| {
                                    structure.get::<gstreamer::Fraction>("framerate").ok()
                                });

                                metadata.interlace_mode = structure.and_then(|structure| {
                                    Some(VideoInterlaceMode::from_string(
                                        structure.get("interlace-mode").ok()?,
                                    ))
                                });

                                metadata.resolution = structure.and_then(|structure| {
                                    Some((
                                        structure.get::<i32>("width").ok()? as usize,
                                        structure.get::<i32>("height").ok()? as usize,
                                    ))
                                });
                            } else {
                                metadata.framerate = None;
                                metadata.interlace_mode = None;
                                metadata.resolution = None;
                            }
                        }
                    }

                    Some(())
                };

                handle_msg(bus, msg);

                gstreamer::BusSyncReply::Drop
            },
            None,
            if self.video_scale.enabled {
                Some(self.video_scale.scale)
            } else {
                None
            },
            gstreamer::Fraction::from(30),
            Some(move |p: Result<gstreamer::Pipeline, PipelineError>| {
                if let Err(e) = p {
                    *pipeline_info_state_for_callback.lock().unwrap() = PipelineStatus::Error(e);
                    ctx_for_callback.request_repaint();
                }
            }),
        )?;

        pipeline.set_state(gstreamer::State::Paused)?;

        Ok(PipelineInfo {
            pipeline,
            state: pipeline_info_state,
            path,
            egui_sink: video_sink.downcast::<elements::EguiSink>().unwrap(),
            at_eos,
            last_seek_pos: ClockTime::ZERO,
            metadata,
        })
    }

    fn create_render_job(
        &mut self,
        ctx: &egui::Context,
        src_path: &Path,
        settings: RenderPipelineSettings,
    ) -> Result<RenderJob, GstreamerError> {
        let scale = if self.video_scale.enabled {
            Some(self.video_scale.scale)
        } else {
            None
        };

        let still_image_settings = {
            let framerate = self
                .pipeline
                .as_ref()
                .map(|info| info.metadata.lock().unwrap())
                .and_then(|metadata: std::sync::MutexGuard<PipelineMetadata>| metadata.framerate)
                .unwrap_or(gstreamer::Fraction::from(30));

            StillImageSettings {
                framerate,
                duration: self.render_settings.duration,
            }
        };

        RenderJob::create(
            &self.executor.make_spawner(),
            ctx,
            src_path,
            settings,
            &still_image_settings,
            scale,
        )
    }

    fn update_effect(&self) {
        if let Some(PipelineInfo { egui_sink, .. }) = &self.pipeline {
            let effect_settings = if self.easy_mode_enabled {
                (&self.easy_mode_settings).into()
            } else {
                self.effect_settings.clone()
            };
            egui_sink.set_property("settings", NtscFilterSettings(effect_settings));
        }
    }

    pub fn set_effect_settings(&mut self, effect_settings: NtscEffect) {
        self.effect_settings = effect_settings;
        self.update_effect();
    }

    fn ensure_single_file_dropped(
        &self,
        files: Option<Vec<DroppedFileHandle>>,
    ) -> Option<DroppedFileHandle> {
        files.and_then(|mut files| {
            let file = files.pop()?;
            if !files.is_empty() {
                self.handle_error(&ApplicationError::DroppedMultipleFiles);
                return None;
            }
            Some(file)
        })
    }

    pub fn handle_error(&self, err: &dyn Error) {
        *self.last_error.borrow_mut() = Some(format!("{}", err));
    }

    pub fn handle_result<T, E: Error>(&self, result: Result<T, E>) {
        if let Err(err) = result {
            self.handle_error(&err);
        }
    }

    fn undo(&mut self) {
        if let Some(new_state) = self.undoer.undo(&self.effect_settings).cloned() {
            self.set_effect_settings(new_state);
        }
    }

    fn redo(&mut self) {
        if let Some(new_state) = self.undoer.redo(&self.effect_settings).cloned() {
            self.set_effect_settings(new_state);
        }
    }
}

fn parse_expression_string(input: &str) -> Option<f64> {
    eval_expression_string(input).ok()
}

impl NtscApp {
    fn setting_from_descriptor<T: Settings>(
        ui: &mut egui::Ui,
        effect_settings: &mut T,
        descriptor: &SettingDescriptor<T>,
        interlace_mode: VideoInterlaceMode,
    ) -> (Response, bool) {
        let mut changed = false;
        if descriptor.id.id == setting_id::RANDOM_SEED.id
            || descriptor.id.id == easy::setting_id::RANDOM_SEED.id
        {
            let resp = ui
                .horizontal(|ui| {
                    let mut value = effect_settings.get_field::<i32>(&descriptor.id).unwrap();
                    let rand_btn_width = ui.spacing().interact_size.y + 4.0;
                    let resp = ui.add_sized(
                        egui::vec2(
                            ui.spacing().slider_width + ui.spacing().interact_size.x
                                - rand_btn_width,
                            ui.spacing().interact_size.y,
                        ),
                        egui::DragValue::new(&mut value).range(i32::MIN..=i32::MAX),
                    );

                    if ui
                        .add_sized(
                            egui::vec2(rand_btn_width, ui.spacing().interact_size.y),
                            egui::Button::new("🎲"),
                        )
                        .on_hover_text("Randomize seed")
                        .clicked()
                    {
                        value = getrandom::u32().unwrap() as i32;
                        changed = true;
                    }

                    changed |= resp.changed();

                    let label = ui.add(egui::Label::new(descriptor.label).truncate());
                    if let Some(description) = descriptor.description {
                        label.on_hover_text(description);
                    }

                    if changed {
                        effect_settings
                            .set_field::<i32>(&descriptor.id, value)
                            .unwrap();
                    }

                    // Return the DragValue response because that's what we want to add the tooltip to
                    resp
                })
                .response;

            return (resp, changed);
        }

        let resp = match &descriptor {
            SettingDescriptor {
                kind: SettingKind::Enumeration { options, .. },
                ..
            } => {
                let selected_index = effect_settings
                    .get_field::<SettingsEnumValue>(&descriptor.id)
                    .unwrap()
                    .0;

                let selected_item = options
                    .iter()
                    .find(|option| option.index == selected_index)
                    .unwrap();
                egui::ComboBox::new(&descriptor.id, descriptor.label)
                    .selected_text(selected_item.label)
                    .show_ui(ui, |ui| {
                        for item in options {
                            let mut label =
                                ui.selectable_label(selected_index == item.index, item.label);

                            if let Some(desc) = item.description {
                                label = label.on_hover_text(desc);
                            }

                            if label.clicked() {
                                effect_settings
                                    .set_field(&descriptor.id, SettingsEnumValue(item.index))
                                    .unwrap();
                                // a selectable_label being clicked doesn't set response.changed
                                changed = true;
                            };
                        }
                    })
                    .response
            }
            SettingDescriptor {
                kind: SettingKind::Percentage { logarithmic, .. },
                ..
            } => {
                let mut value = effect_settings.get_field::<f32>(&descriptor.id).unwrap();

                let slider: Response = ui.add(
                    egui::Slider::new(&mut value, 0.0..=1.0)
                        .text(descriptor.label)
                        .custom_parser(parse_expression_string)
                        .custom_formatter(format_percentage)
                        .logarithmic(*logarithmic),
                );

                if slider.changed() {
                    let _ = effect_settings.set_field(&descriptor.id, value);
                }

                slider
            }
            SettingDescriptor {
                kind: SettingKind::IntRange { range, .. },
                ..
            } => {
                let mut value = effect_settings.get_field::<i32>(&descriptor.id).unwrap();

                let slider = ui.add(
                    egui::Slider::new(&mut value, range.clone())
                        .text(descriptor.label)
                        .custom_parser(parse_expression_string),
                );

                if slider.changed() {
                    effect_settings
                        .set_field::<i32>(&descriptor.id, value)
                        .unwrap();
                }

                slider
            }
            SettingDescriptor {
                kind:
                    SettingKind::FloatRange {
                        range, logarithmic, ..
                    },
                ..
            } => {
                let mut value = effect_settings.get_field::<f32>(&descriptor.id).unwrap();

                let slider = ui.add(
                    egui::Slider::new(&mut value, range.clone())
                        .text(descriptor.label)
                        .custom_parser(parse_expression_string)
                        .logarithmic(*logarithmic),
                );

                if slider.changed() {
                    let _ = effect_settings.set_field(&descriptor.id, value);
                }

                slider
            }
            SettingDescriptor {
                kind: SettingKind::Boolean,
                ..
            } => {
                let mut value = effect_settings.get_field::<bool>(&descriptor.id).unwrap();

                let checkbox = ui.checkbox(&mut value, descriptor.label);

                if checkbox.changed() {
                    let _ = effect_settings.set_field(&descriptor.id, value);
                }

                checkbox
            }
            SettingDescriptor {
                kind: SettingKind::Group { children, .. },
                id,
                ..
            } => {
                ui.add_space(2.0);
                let resp = ui
                    .group(|ui| {
                        ui.set_width(ui.max_rect().width());
                        let mut checked =
                            effect_settings.get_field::<bool>(&descriptor.id).unwrap();
                        let was_checked = checked;

                        let id = ui.make_persistent_id(id);
                        let mut state =
                            egui::collapsing_header::CollapsingState::load_with_default_open(
                                ui.ctx(),
                                id,
                                checked,
                            );

                        let checkbox = ui
                            .horizontal(|ui| {
                                let checkbox = ui.checkbox(&mut checked, descriptor.label);

                                if checkbox.changed() {
                                    let _ = effect_settings.set_field(&descriptor.id, checked);
                                }

                                // Show twirly arrow at the rightmost position
                                let rect = ui.max_rect();
                                let rect = rect.with_min_x(rect.max.x - rect.height());
                                let response = ui.allocate_rect(rect, egui::Sense::click());
                                egui::collapsing_header::paint_default_icon(
                                    ui,
                                    state.openness(ui.ctx()),
                                    // The default arrow size is big and distracting, but we want to keep the large
                                    // hitbox, so only shrink the response for painting.
                                    &response.clone().with_new_rect(response.rect.shrink(2.0)),
                                );

                                if response.clicked() {
                                    state.toggle(ui);
                                }

                                checkbox
                            })
                            .inner;

                        if !checked {
                            ui.disable();
                        }

                        // When a settings group is re-enabled, expand it automatically.
                        if checked && !was_checked {
                            state.set_open(true);
                        }

                        let child_response = state.show_body_unindented(ui, |ui| {
                            Self::settings_from_descriptors(
                                effect_settings,
                                ui,
                                children,
                                interlace_mode,
                            )
                        });

                        if let Some(egui::InnerResponse { inner, .. }) = child_response {
                            changed |= inner;
                        }

                        checkbox
                    })
                    .inner;
                ui.add_space(2.0);
                resp
            }
        };

        (resp, changed)
    }

    fn settings_from_descriptors<T: Settings>(
        effect_settings: &mut T,
        ui: &mut egui::Ui,
        descriptors: &[SettingDescriptor<T>],
        interlace_mode: VideoInterlaceMode,
    ) -> bool {
        let mut changed = false;
        for descriptor in descriptors {
            // The "Use field" setting has no effect on interlaced video.
            let (response, setting_changed) = if (descriptor.id.id == setting_id::USE_FIELD.id
                || descriptor.id.id == easy::setting_id::USE_FIELD.id)
                && interlace_mode != VideoInterlaceMode::Progressive
            {
                let resp = ui.add_enabled_ui(false, |ui| {
                    Self::setting_from_descriptor(
                        ui,
                        effect_settings,
                        descriptor,
                        VideoInterlaceMode::Progressive,
                    )
                });

                resp.inner
            } else {
                Self::setting_from_descriptor(ui, effect_settings, descriptor, interlace_mode)
            };

            changed |= response.changed() || setting_changed;

            if let Some(desc) = descriptor.description {
                response.on_hover_text(desc);
            }
        }

        changed
    }

    fn show_effect_settings(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        egui::Panel::bottom("preset_copy_paste")
            .interact_height_tall(ui)
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    if ui.button("Save to...").clicked() {
                        let settings_list = self.settings_list.clone();
                        let effect_settings = self.effect_settings.clone();
                        let handle = rfd::AsyncFileDialog::new()
                            .set_parent(frame)
                            .add_filter("ntsc-rs preset", &["json"])
                            .set_file_name("settings.json")
                            .save_file();
                        self.spawn(async move {
                            let handle = handle.await;
                            let handle = match handle {
                                Some(h) => h,
                                None => return None,
                            };

                            Some(Box::new(move |_: &mut NtscApp| {
                                let file =
                                    File::create(handle.path()).context(CreatePresetFileSnafu)?;
                                settings_list
                                    .write_json_to_io(&effect_settings, file)
                                    .context(CreatePresetJSONSnafu)?;
                                Ok(())
                            }) as _)
                        });
                    }

                    if ui.button("Load from...").clicked() {
                        let handle = rfd::AsyncFileDialog::new()
                            .set_parent(frame)
                            .add_filter("ntsc-rs preset", &["json"])
                            .pick_file();
                        self.spawn(async move {
                            let handle = handle.await;

                            Some(Box::new(
                                move |app: &mut NtscApp| -> Result<(), ApplicationError> {
                                    let handle = match handle {
                                        Some(h) => h,
                                        // user cancelled the operation
                                        None => return Ok(()),
                                    };

                                    let mut file =
                                        File::open(handle.path()).context(JSONReadSnafu)?;

                                    let mut buf = String::new();
                                    file.read_to_string(&mut buf).context(JSONReadSnafu)?;

                                    let settings = app
                                        .settings_list
                                        .from_json(&buf)
                                        .context(JSONParseSnafu)?;

                                    app.set_effect_settings(settings);

                                    Ok(())
                                },
                            ) as _)
                        });
                    }

                    if ui.button("📋 Copy").clicked() {
                        let json_string = self.settings_list.to_json_string(&self.effect_settings);
                        match json_string {
                            Ok(json) => {
                                ui.ctx().send_cmd(egui::OutputCommand::CopyText(json));
                            }
                            Err(e) => {
                                self.handle_error(&e);
                            }
                        }
                    }

                    let btn = ui.button("📄 Paste");

                    let paste_popup_id = ui.make_persistent_id("paste_popup_open");

                    if btn.clicked() {
                        ui.ctx().data_mut(|map| {
                            let old_value =
                                map.get_temp_mut_or_insert_with(paste_popup_id, || false);
                            *old_value = !*old_value;
                        });
                    }

                    if ui
                        .ctx()
                        .data(|map| map.get_temp(paste_popup_id).unwrap_or(false))
                    {
                        let mut is_open = true;
                        egui::Window::new("Paste JSON")
                            .default_pos(btn.rect.center_top())
                            .open(&mut is_open)
                            .show(ui.ctx(), |ui| {
                                ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                                    if ui.button("Load").clicked() {
                                        match self
                                            .settings_list
                                            .from_json(&self.settings_json_paste)
                                        {
                                            Ok(settings) => {
                                                self.set_effect_settings(settings);
                                                // Close the popup if the JSON was successfully loaded
                                                ui.ctx().data_mut(|map| {
                                                    map.insert_temp(paste_popup_id, false)
                                                });
                                            }
                                            Err(e) => {
                                                self.handle_error(&e);
                                            }
                                        }
                                    }
                                    ui.with_layout(
                                        egui::Layout::top_down(egui::Align::Min),
                                        |ui| {
                                            egui::ScrollArea::new([false, true])
                                                .auto_shrink([true, false])
                                                .show(ui, |ui| {
                                                    ui.add_sized(
                                                        ui.available_size(),
                                                        egui::TextEdit::multiline(
                                                            &mut self.settings_json_paste,
                                                        ),
                                                    );
                                                });
                                        },
                                    );
                                });
                            });

                        if !is_open {
                            ui.ctx()
                                .data_mut(|map| map.insert_temp(paste_popup_id, false));
                        }
                    }

                    if ui.button("Reset").clicked() {
                        self.set_effect_settings(NtscEffect::default());
                        self.presets_state.deselect_preset();
                    }
                });
            });

        let id = ui.make_persistent_id("presets_manager_open");
        let collapse_state =
            egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false);
        egui::Panel::bottom("preset_manager")
            .resizable(collapse_state.is_open())
            // Determined experimentally to prevent the presets manager from becoming too tall to drag back down by its
            // top edge
            .max_size(600.0f32.min(ui.available_height() - 100.0))
            .min_size(egui::lerp(
                ui.spacing().interact_size.y..=200.0,
                collapse_state.openness(ui.ctx()),
            ))
            .show(ui, |ui| {
                let collapse_state = collapse_state.show_header(ui, |ui| {
                    // In order to properly resize the panel when we open the "Presets" header, we need to create the
                    // CollapsingState outside this UI. That means we can't just use a regular CollapsingHeader and must
                    // draw it ourselves. Somehow, this `interact` causes the header to be clickable throughout.
                    let resp =
                        ui.interact(ui.available_rect_before_wrap(), id, egui::Sense::click());
                    let style = ui.style().interact(&resp);
                    ui.add(
                        egui::Label::new(egui::RichText::new("Presets").color(style.text_color()))
                            .selectable(false),
                    );
                });

                collapse_state.body_unindented(|ui| {
                    if let Some(dropped_presets) = ui.show_dnd_overlay("Drop to install presets") {
                        self.install_presets(
                            dropped_presets
                                .into_iter()
                                .map(|file| file.path().to_owned()),
                        );
                    }

                    self.show_presets_pane(ui);
                });
            });

        egui::CentralPanel::default().show(ui, |ui| {
            if let Some(handle) =
                self.ensure_single_file_dropped(ui.show_dnd_overlay("Drop to load preset"))
            {
                self.load_preset(handle.path().to_owned());
            }

            egui::ScrollArea::vertical()
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    Self::setup_control_rows(ui);

                    ui.horizontal(|ui| {
                        let scale_checkbox = ui
                            .checkbox(&mut self.video_scale.enabled, "Scale to")
                            .on_hover_text(
                                "Scale the video prior to applying the effect. Real NTSC footage \
                                 is 480 lines tall. This applies to both the preview and the \
                                 final render, and is not saved as part of presets.",
                            );
                        ui.add_enabled_ui(self.video_scale.enabled, |ui| {
                            let drag_resp = ui.add(
                                egui::DragValue::new(&mut self.video_scale.scale.scanlines)
                                    .range(1..=usize::MAX),
                            );
                            ui.label("lines");
                            let mut filter_changed = false;
                            let filter_resp = egui::ComboBox::from_id_salt("video_scale_filter")
                                .selected_text(self.video_scale.scale.filter.label_and_tooltip().0)
                                .show_ui(ui, |ui| {
                                    for value in VideoScaleFilter::values() {
                                        let (label_text, tooltip) = value.label_and_tooltip();
                                        let label = ui
                                            .selectable_label(
                                                *value == self.video_scale.scale.filter,
                                                label_text,
                                            )
                                            .on_hover_text(tooltip);

                                        if label.clicked() {
                                            filter_changed = true;
                                            self.video_scale.scale.filter = *value;
                                        }
                                    }
                                });
                            filter_resp.response.on_hover_text("Resizing filter");
                            if (drag_resp.changed() || scale_checkbox.changed() || filter_changed)
                                && let Some(info) = &self.pipeline
                            {
                                let res = info.pipeline.rescale_video(
                                    info.last_seek_pos,
                                    if self.video_scale.enabled {
                                        Some(self.video_scale.scale)
                                    } else {
                                        None
                                    },
                                );
                                self.handle_result(res);
                            }
                        });
                    });

                    let Self {
                        settings_list,
                        effect_settings,
                        settings_list_easy,
                        easy_mode_settings,
                        pipeline,
                        ..
                    } = self;
                    let interlace_mode = pipeline
                        .as_ref()
                        .and_then(|pipeline| pipeline.metadata.lock().unwrap().interlace_mode)
                        .unwrap_or(VideoInterlaceMode::Progressive);

                    ui.separator();

                    let mut settings_changed = false;

                    if EXPERIMENTAL_EASY_MODE {
                        settings_changed |= ui
                            .checkbox(&mut self.easy_mode_enabled, "Easy mode")
                            .changed();
                    }

                    if self.easy_mode_enabled {
                        settings_changed |= Self::settings_from_descriptors(
                            easy_mode_settings,
                            ui,
                            &settings_list_easy.setting_descriptors,
                            interlace_mode,
                        );
                    } else {
                        settings_changed |= Self::settings_from_descriptors(
                            effect_settings,
                            ui,
                            &settings_list.setting_descriptors,
                            interlace_mode,
                        );
                    }
                    if settings_changed {
                        self.update_effect();
                    }
                });
        });
    }

    fn setup_control_rows(ui: &mut egui::Ui) {
        const LABEL_WIDTH: f32 = 180.0;

        // Subtract 2.0 to avoid some weird egui bug (https://github.com/emilk/egui/pull/8056)
        let remaining_width = ui.max_rect().width() - LABEL_WIDTH - 2.0;

        let spacing = ui.spacing_mut();
        spacing.slider_width = remaining_width - 48.0;
        spacing.interact_size.x = 48.0;
        spacing.combo_width =
            spacing.slider_width + spacing.interact_size.x + spacing.item_spacing.x;
    }

    fn show_render_settings(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        egui::Frame::central_panel(ui.style()).show(ui, |ui| {
            Self::setup_control_rows(ui);
            let mut codec_changed = false;
            egui::ComboBox::from_label("Codec")
                .selected_text(self.render_settings.output_codec.label())
                .show_ui(ui, |ui| {
                    let mut item = |item: OutputCodec| {
                        codec_changed |= ui
                            .selectable_value(
                                &mut self.render_settings.output_codec,
                                item,
                                item.label(),
                            )
                            .changed();
                    };
                    item(OutputCodec::H264);
                    item(OutputCodec::Ffv1);
                    item(OutputCodec::PngSequence);
                });

            if codec_changed {
                self.render_settings
                    .output_path
                    .set_extension(self.render_settings.output_codec.extension());
            }

            match self.render_settings.output_codec {
                OutputCodec::H264 => {
                    ui.add(
                        egui::Slider::new(
                            &mut self.render_settings.h264_settings.quality,
                            H264Settings::QUALITY_RANGE,
                        )
                        .text("Quality"),
                    )
                    .on_hover_text(
                        "Video quality factor, where 0 is the worst quality and 50 is the best. \
                         Higher quality videos take up more space.",
                    );
                    ui.add(
                        egui::Slider::new(
                            &mut self.render_settings.h264_settings.encode_speed,
                            H264Settings::ENCODE_SPEED_RANGE,
                        )
                        .text("Encoding speed"),
                    )
                    .on_hover_text(
                        "Encoding speed preset. Higher encoding speeds provide a worse \
                         compression ratio, resulting in larger videos at a given quality.",
                    );
                    // Disabled for now until I can find a way to query for 10-bit support
                    /*ui.checkbox(
                        &mut self.render_settings.h264_settings.ten_bit,
                        "10-bit color",
                    );*/
                    ui.checkbox(
                        &mut self.render_settings.h264_settings.chroma_subsampling,
                        "4:2:0 chroma subsampling",
                    )
                    .on_hover_text(
                        "Subsample the chrominance planes to half the resolution of the luminance \
                         plane. Increases playback compatibility.",
                    );
                }

                OutputCodec::Ffv1 => {
                    egui::ComboBox::from_label("Bit depth")
                        .selected_text(self.render_settings.ffv1_settings.bit_depth.label())
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.render_settings.ffv1_settings.bit_depth,
                                Ffv1BitDepth::Bits8,
                                Ffv1BitDepth::Bits8.label(),
                            );
                            ui.selectable_value(
                                &mut self.render_settings.ffv1_settings.bit_depth,
                                Ffv1BitDepth::Bits10,
                                Ffv1BitDepth::Bits10.label(),
                            );
                            ui.selectable_value(
                                &mut self.render_settings.ffv1_settings.bit_depth,
                                Ffv1BitDepth::Bits12,
                                Ffv1BitDepth::Bits12.label(),
                            );
                        });

                    ui.checkbox(
                        &mut self.render_settings.ffv1_settings.chroma_subsampling,
                        "4:2:0 chroma subsampling",
                    )
                    .on_hover_text(
                        "Subsample the chrominance planes to half the resolution of the luminance \
                         plane. Results in smaller files.",
                    );
                }

                OutputCodec::PngSequence => {
                    ui.add(
                        egui::Slider::new(
                            &mut self.render_settings.png_sequence_settings.compression_level,
                            PngSequenceSettings::COMPRESSION_LEVEL_RANGE,
                        )
                        .text("Compression level"),
                    )
                    .on_hover_text(
                        "Compression level for PNG encoding. Higher compression levels produce \
                         smaller files but take longer to render.",
                    );
                }
            }

            ui.separator();

            ui.rtl(|ui| {
                let save_file = ui.button("📁").on_hover_text("Browse for a path").clicked();

                ui.ltr(|ui| {
                    ui.label("Destination file:");
                    let mut path = self.render_settings.output_path.to_string_lossy();
                    if ui
                        .add_sized(ui.available_size(), egui::TextEdit::singleline(&mut path))
                        .changed()
                    {
                        self.render_settings.output_path = PathBuf::from(OsStr::new(path.as_ref()));
                    }
                });

                if save_file {
                    let output_path = self.render_settings.output_path.as_path();
                    let mut file_dialog = rfd::AsyncFileDialog::new().set_parent(frame);

                    if output_path.as_os_str().is_empty() {
                        // By default, name the output file [source filename]_ntsc.[ext] (for video files) or [source
                        // filename]_####.[ext] (for image sequences) and put it in the same directory as the source
                        // file.
                        if let Some(PipelineInfo {
                            path: source_path, ..
                        }) = &self.pipeline
                        {
                            if let Some(parent) = source_path.parent() {
                                file_dialog = file_dialog.set_directory(parent);
                            }
                            if let Some(file_stem) = source_path.file_stem() {
                                let mut file_name = file_stem.to_string_lossy().into_owned();
                                if self.render_settings.output_codec.is_image_sequence() {
                                    file_name.push_str("_####.");
                                } else {
                                    file_name.push_str("_ntsc.");
                                }
                                file_name.push_str(self.render_settings.output_codec.extension());
                                file_dialog = file_dialog.set_file_name(file_name);
                            }
                        }
                    } else {
                        if let Some(parent) = output_path.parent() {
                            file_dialog = file_dialog.set_directory(parent);
                        }
                        if let Some(file_name) = output_path.file_name() {
                            file_dialog = file_dialog.set_file_name(file_name.to_string_lossy());
                        }
                    }

                    let file_dialog = file_dialog.save_file();
                    let output_codec = self.render_settings.output_codec;
                    self.spawn(async move {
                        let handle = file_dialog.await?;
                        let mut output_path: PathBuf = handle.into();

                        Some(Box::new(move |app: &mut NtscApp| {
                            if output_path.extension().is_none() {
                                output_path.set_extension(output_codec.extension());
                            }
                            app.render_settings.output_path = output_path;

                            Ok(())
                        }) as _)
                    });
                }
            });

            let src_path = self.pipeline.as_ref().map(|info| &info.path);

            let mut duration = self.render_settings.duration.mseconds();
            if self
                .pipeline
                .as_ref()
                .map(|info| info.metadata.lock().unwrap())
                .and_then(|metadata| metadata.is_still_image)
                .unwrap_or(false)
            {
                ui.horizontal(|ui| {
                    ui.label("Duration:");
                    if ui
                        .add(
                            egui::DragValue::new(&mut duration)
                                .custom_formatter(|value, _| {
                                    clock_time_format(
                                        (value * ClockTime::MSECOND.nseconds() as f64) as u64,
                                    )
                                })
                                .custom_parser(clock_time_parser)
                                .speed(100.0),
                        )
                        .changed()
                    {
                        self.render_settings.duration = ClockTime::from_mseconds(duration);
                    }
                });
            }

            ui.add_enabled(
                self.effect_settings.use_field.interlaced_output_allowed()
                    && self.render_settings.interlaced_output_allowed(),
                egui::Checkbox::new(&mut self.render_settings.interlaced, "Interlaced output"),
            )
            .on_disabled_hover_text(
                if !self.render_settings.interlaced_output_allowed() {
                    "Image sequences do not support interlaced output."
                } else {
                    "To enable interlaced output, set the \"Use field\" setting to \"Interleaved\"."
                },
            );

            if ui
                .add_enabled(
                    !self.render_settings.output_path.as_os_str().is_empty() && src_path.is_some(),
                    egui::Button::new("Render"),
                )
                .clicked()
            {
                let effect_settings = self.effect_settings.clone();
                let render_settings = self.render_settings.clone();
                let output_codec = render_settings.output_codec;
                let output_dir_path = render_settings
                    .output_path
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_default();
                let ctx = ui.ctx().clone();
                let src_path = src_path.cloned();
                self.spawn(async move {
                    let is_empty = if output_codec.is_image_sequence() {
                        match unblock(move || output_dir_path.read_dir()).await {
                            Ok(mut read_dir) => unblock(move || read_dir.next()).await.is_none(),
                            Err(e) => {
                                return Some(
                                    Box::new(move |_: &mut NtscApp| Err(e).context(FsSnafu)) as _,
                                );
                            }
                        }
                    } else {
                        true
                    };

                    Some(Box::new(move |app: &mut NtscApp| {
                        let render_job = move |app: &mut NtscApp| {
                            app.create_render_job(
                                &ctx,
                                &src_path.unwrap(),
                                RenderPipelineSettings::from_gui_settings(
                                    &effect_settings,
                                    &render_settings,
                                ),
                            )
                            .context(CreateRenderJobSnafu)
                        };
                        if is_empty {
                            let render_job = render_job(app)?;
                            app.render_jobs.push(render_job);
                            Ok(())
                        } else {
                            app.image_sequence_dialog_queued_render_job =
                                Some(Box::new(render_job) as _);
                            Ok(())
                        }
                    }) as _)
                });
            }

            ui.separator();

            let mut render_job_error = None;
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    self.render_jobs.retain_mut(|job| {
                        let RenderJobResponse { closed, error, .. } =
                            RenderJobWidget::new(job).show(ui);
                        if let Some(error) = error {
                            render_job_error = Some(error);
                        }
                        !closed
                    })
                });

            if let Some(e) = render_job_error {
                self.handle_error(&e);
            }
        });
    }

    fn show_video_pane(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let last_seek_pos = if let Some(info) = &mut self.pipeline {
            // While seeking, GStreamer sometimes doesn't return a timecode. In that case, use the last timecode it
            // did respond with.
            let queried_pos = info.pipeline.query_position::<ClockTime>();
            if let Some(position) = queried_pos {
                info.last_seek_pos = position;
            }
            info.last_seek_pos
        } else {
            ClockTime::ZERO
        };

        let framerate = (|| {
            let caps = self
                .pipeline
                .as_ref()?
                .pipeline
                .inner
                .by_name("video_queue")?
                .static_pad("sink")?
                .current_caps()?;
            let framerate = caps
                .structure(0)?
                .get::<gstreamer::Fraction>("framerate")
                .ok()?;

            // If the framerate is 0, treat it like it's absent
            (framerate.numer() != 0).then_some(framerate)
        })();

        egui::Panel::top("video_info")
            .interact_height(ui)
            .show(ui, |ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let mut remove_pipeline = false;
                    let mut change_framerate_res = None;
                    let mut save_image_to: Option<(PathBuf, PathBuf)> = None;
                    let mut copy_image_res: Option<Result<ColorImage, GstreamerError>> = None;
                    if let Some(info) = &mut self.pipeline {
                        let mut metadata = info.metadata.lock().unwrap();
                        if ui.button("🗙").clicked() {
                            remove_pipeline = true;
                        }

                        ui.separator();

                        if ui.button("Save frame").clicked() {
                            let src_path = info.path.clone();

                            let dst_path = src_path.with_extension("");
                            save_image_to = Some((src_path, dst_path));
                        }

                        if ui.button("Copy frame").clicked() {
                            let egui_sink =
                                info.egui_sink.downcast_ref::<elements::EguiSink>().unwrap();

                            let egui_sink = EguiSink::from_obj(egui_sink);
                            copy_image_res = Some(egui_sink.get_image().map_err(|e| e.into()));
                        }

                        if let Some(current_framerate) = metadata.framerate {
                            ui.separator();
                            match metadata.is_still_image {
                                Some(true) => {
                                    let mut new_framerate = current_framerate.numer() as f64
                                        / current_framerate.denom() as f64;
                                    ui.label("fps");
                                    if ui
                                        .add(
                                            egui::DragValue::new(&mut new_framerate)
                                                .range(0.0..=240.0),
                                        )
                                        .changed()
                                    {
                                        let framerate_fraction =
                                            gstreamer::Fraction::approximate_f64(new_framerate);
                                        if let Some(f) = framerate_fraction {
                                            let changed_framerate =
                                                info.pipeline.set_still_image_framerate(f);
                                            if let Ok(Some(new_framerate)) = changed_framerate {
                                                metadata.framerate = Some(new_framerate);
                                            }

                                            change_framerate_res = Some(changed_framerate);
                                        }
                                    }
                                }
                                Some(false) => {
                                    let mut fps_display = format!(
                                        "{:.2} fps",
                                        current_framerate.numer() as f64
                                            / current_framerate.denom() as f64
                                    );
                                    if let Some(interlace_mode) = metadata.interlace_mode {
                                        fps_display.push_str(match interlace_mode {
                                            VideoInterlaceMode::Progressive => " (progressive)",
                                            VideoInterlaceMode::Interleaved => " (interlaced)",
                                            VideoInterlaceMode::Mixed => " (telecined)",
                                            _ => "",
                                        });
                                    }
                                    ui.label(fps_display);
                                }
                                None => {}
                            }
                        }

                        if let Some((width, height)) = metadata.resolution {
                            ui.separator();
                            ui.label(format!("{}x{}", width, height));
                        }

                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            ui.add(egui::Label::new(info.path.to_string_lossy()).truncate());
                        });
                    }

                    if let Some(res) = change_framerate_res {
                        self.handle_result(res);
                    }

                    if let Some(res) = copy_image_res {
                        match res {
                            Ok(image) => {
                                ui.ctx().send_cmd(egui::OutputCommand::CopyImage(image));
                            }
                            Err(e) => {
                                self.handle_error(&e);
                            }
                        }
                    }

                    if remove_pipeline {
                        self.close_video(ui.ctx())
                    }

                    if let Some((src_path, dst_path)) = save_image_to {
                        let ctx = ui.ctx().clone();
                        let handle = rfd::AsyncFileDialog::new()
                            .set_parent(frame)
                            .set_directory(dst_path.parent().unwrap_or(Path::new("/")))
                            .set_file_name(format!(
                                "{}_ntsc.png",
                                dst_path.file_name().to_owned().unwrap().to_string_lossy()
                            ))
                            .save_file();
                        self.spawn(async move {
                            handle.await.map(|handle| {
                                Box::new(move |app: &mut NtscApp| {
                                    let current_time = app
                                        .pipeline
                                        .as_ref()
                                        .and_then(|info| {
                                            info.pipeline.query_position::<ClockTime>()
                                        })
                                        .unwrap_or(ClockTime::ZERO);

                                    let res = app.create_render_job(
                                        &ctx,
                                        &src_path.clone(),
                                        RenderPipelineSettings {
                                            codec_settings: RenderPipelineCodec::Png(PngSettings {
                                                seek_to: current_time,
                                                settings: Default::default(),
                                            }),
                                            output_path: handle.into(),
                                            interlacing: RenderInterlaceMode::Progressive,
                                            effect_settings: app.effect_settings.clone(),
                                        },
                                    );
                                    if let Ok(job) = res {
                                        app.render_jobs.push(job);
                                    } else {
                                        app.handle_result(res);
                                    }
                                    Ok(())
                                }) as _
                            })
                        });
                    }
                });
            });

        egui::Panel::bottom("video_controls")
            .interact_height_tall(ui)
            .show(ui, |ui| {
                if self.pipeline.is_none() {
                    ui.disable();
                }
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let btn_widget = egui::Button::new(match &self.pipeline {
                        Some(PipelineInfo { pipeline, .. }) => {
                            let state = pipeline.current_state();
                            match state {
                                gstreamer::State::Paused | gstreamer::State::Ready => "▶",
                                gstreamer::State::Playing => "⏸",
                                _ => "▶",
                            }
                        }
                        None => "▶",
                    });
                    let btn = ui.add_sized(
                        vec2(
                            ui.spacing().interact_size.y * 1.5,
                            ui.spacing().interact_size.y * 1.5,
                        ),
                        btn_widget,
                    );

                    if !ui.egui_wants_keyboard_input()
                        && ui.input(|i| {
                            i.events.iter().any(|event| {
                                if let egui::Event::Key {
                                    key,
                                    pressed,
                                    repeat,
                                    modifiers,
                                    ..
                                } = event
                                {
                                    *key == egui::Key::Space
                                        && *pressed
                                        && !repeat
                                        && modifiers.is_none()
                                } else {
                                    false
                                }
                            })
                        })
                    {
                        let res = self.pipeline.as_mut().map(|p| p.toggle_playing());
                        if let Some(res) = res {
                            self.handle_result(res);
                        }
                    }

                    if btn.clicked() {
                        let res = self.pipeline.as_mut().map(|p| p.toggle_playing());
                        if let Some(res) = res {
                            self.handle_result(res);
                        }
                    }

                    let duration = if let Some(info) = &self.pipeline {
                        info.pipeline.query_duration::<ClockTime>()
                    } else {
                        None
                    };

                    let mut timecode_ms =
                        last_seek_pos.nseconds() as f64 / ClockTime::MSECOND.nseconds() as f64;
                    let frame_pace = if let Some(framerate) = framerate {
                        framerate.denom() as f64 / framerate.numer() as f64
                    } else {
                        1f64 / 30f64
                    };

                    let mut drag_value = egui::DragValue::new(&mut timecode_ms)
                        .custom_formatter(|value, _| {
                            clock_time_format((value * ClockTime::MSECOND.nseconds() as f64) as u64)
                        })
                        .custom_parser(clock_time_parser)
                        .speed(frame_pace * 1000.0 * 0.5);

                    if let Some(duration) = duration {
                        drag_value = drag_value.range(0..=duration.mseconds());
                    }

                    if ui.add(drag_value).changed()
                        && let Some(info) = &self.pipeline
                    {
                        // don't use KEY_UNIT here; it causes seeking to often be very inaccurate (almost a second of deviation)
                        let _ = info.pipeline.seek_simple(
                            gstreamer::SeekFlags::FLUSH | gstreamer::SeekFlags::ACCURATE,
                            ClockTime::from_nseconds(
                                (timecode_ms * ClockTime::MSECOND.nseconds() as f64) as u64,
                            ),
                        );
                    }

                    ui.separator();

                    ui.add(egui::Label::new("🔎").selectable(false))
                        .on_hover_text("Zoom preview");
                    ui.add_enabled(
                        !self.video_zoom.fit,
                        egui::DragValue::new(&mut self.video_zoom.scale)
                            .range(0.0..=8.0)
                            .speed(0.01)
                            .custom_formatter(format_percentage)
                            // Treat as a percentage above 8x zoom
                            .custom_parser(|input| parse_decimal_or_percentage(input, 8.0)),
                    );
                    ui.checkbox(&mut self.video_zoom.fit, "Fit");

                    ui.separator();

                    let has_audio = self
                        .pipeline
                        .as_ref()
                        .map(|info| info.metadata.lock().unwrap())
                        .and_then(|metadata| metadata.has_audio)
                        .unwrap_or(false);

                    ui.add_enabled_ui(has_audio, |ui| {
                        let mut update_volume = false;

                        if ui
                            .button(if self.audio_volume.mute {
                                "🔇"
                            } else {
                                match self.audio_volume.gain {
                                    0.0 => "🔇",
                                    0.0..=0.33 => "🔈",
                                    0.0..=0.67 => "🔉",
                                    _ => "🔊",
                                }
                            })
                            .on_hover_text(if self.audio_volume.mute {
                                "Unmute"
                            } else {
                                "Mute"
                            })
                            .clicked()
                        {
                            self.audio_volume.mute = !self.audio_volume.mute;
                            // "<= 0.0" to handle negative zero (not sure if it'll ever happen; better safe than sorry)
                            if !self.audio_volume.mute && self.audio_volume.gain <= 0.0 {
                                // Restore the previous gain after the user mutes by dragging the slider to 0 then unmutes
                                self.audio_volume.gain = self.audio_volume.gain_pre_mute;
                            }
                            update_volume = true;
                        }

                        let resp = ui.add_enabled(
                            !self.audio_volume.mute,
                            egui::Slider::new(&mut self.audio_volume.gain, 0.0..=1.25)
                                // Treat as a percentage above 125% volume
                                .custom_parser(|input| parse_decimal_or_percentage(input, 1.25))
                                .custom_formatter(format_percentage),
                        );

                        if resp.drag_stopped() {
                            if self.audio_volume.gain > 0.0 {
                                // Set the gain to restore after dragging the slider to 0
                                self.audio_volume.gain_pre_mute = self.audio_volume.gain;
                            } else {
                                // Wait for drag release to mute because it disables the slider
                                self.audio_volume.mute = true;
                            }
                        }

                        if resp.changed() || resp.drag_stopped() {
                            update_volume = true;
                        }

                        if update_volume && let Some(pipeline_info) = &self.pipeline {
                            pipeline_info.pipeline.set_volume(
                                // Unlogarithmify volume (at least to my ears, this gives more control at the low end
                                // of the slider)
                                10f64.powf(self.audio_volume.gain - 1.0).max(0.0),
                                self.audio_volume.mute || self.audio_volume.gain == 0.0,
                            );
                        }
                    });

                    ui.separator();

                    let mut update_effect_preview = false;
                    ui.add(egui::Label::new("✨").selectable(false))
                        .on_hover_text("Effect preview");
                    update_effect_preview |= ui
                        .selectable_value(
                            &mut self.effect_preview.mode,
                            EffectPreviewMode::Enabled,
                            "Enable",
                        )
                        .changed();
                    update_effect_preview |= ui
                        .selectable_value(
                            &mut self.effect_preview.mode,
                            EffectPreviewMode::Disabled,
                            "Disable",
                        )
                        .changed();
                    update_effect_preview |= ui
                        .selectable_value(
                            &mut self.effect_preview.mode,
                            EffectPreviewMode::SplitScreen,
                            "Split",
                        )
                        .changed();

                    if update_effect_preview
                        && let Some(PipelineInfo { egui_sink, .. }) = &self.pipeline
                    {
                        egui_sink.set_property(
                            "preview-mode",
                            Self::sink_preview_mode(&self.effect_preview),
                        );
                    }
                });
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::side_top_panel(ui.style()).inner_margin(0.0))
            .show(ui, |ui| {
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                    if let Some(info) = &mut self.pipeline {
                        let mut timecode = info.last_seek_pos.nseconds();

                        let duration = info.pipeline.query_duration::<ClockTime>();

                        if let Some(duration) = duration
                            && ui
                                .add(Timeline::new(
                                    &mut timecode,
                                    0..=duration.nseconds(),
                                    framerate,
                                ))
                                .changed()
                        {
                            let _ = info.pipeline.seek_simple(
                                gstreamer::SeekFlags::FLUSH | gstreamer::SeekFlags::ACCURATE,
                                ClockTime::from_nseconds(timecode),
                            );
                        }
                    }
                    egui::ScrollArea::both()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            if let Some(media) = self.ensure_single_file_dropped(
                                ui.show_dnd_overlay("Drop to load media"),
                            ) {
                                let res = self.load_video(ui.ctx(), media.path().to_owned());
                                self.handle_result(res);
                            }
                            ui.with_layout(
                                egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
                                |ui| {
                                    let Some(PipelineInfo { egui_sink, .. }) = &mut self.pipeline
                                    else {
                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new("No media loaded").heading(),
                                            )
                                            .selectable(false),
                                        );
                                        return;
                                    };

                                    let texture = egui_sink.property::<SinkTexture>("texture");

                                    let Some(mut preview) = texture.handle else {
                                        ui.add(egui::Spinner::new());
                                        return;
                                    };

                                    let par = {
                                        let par = texture
                                            .pixel_aspect_ratio
                                            .unwrap_or(Fraction::from_integer(1));
                                        (par.numer() as f32, par.denom() as f32)
                                    };

                                    let logical_size = {
                                        let mut preview_size = preview.size_vec2();

                                        preview_size[0] = (preview_size[0] * par.0) / par.1;

                                        preview_size
                                    };

                                    /**/

                                    let texture_size = if self.video_scale.enabled {
                                        let texture_actual_size = logical_size;
                                        // scale_factor is usually 1.0, but while the user is dragging the "Video scale"
                                        // value, gstreamer may take a bit to update the video scale.
                                        let scale_factor = self.video_scale.scale.scanlines as f32
                                            / texture_actual_size.y;
                                        vec2(
                                            (texture_actual_size.x * scale_factor).round(),
                                            self.video_scale.scale.scanlines as f32,
                                        )
                                    } else {
                                        logical_size
                                    };
                                    let scale_factor = if self.video_zoom.fit {
                                        // Due to floating-point error, a scrollbar may appear even if we scale down. To
                                        // avoid a spurious scrollbar, we need to subtract 2 (TODO: this used to be 1,
                                        // but in egui 0.31, it needs to be 2 for some reason. Why?)
                                        ((ui.available_size() - vec2(2.0, 2.0)) / texture_size)
                                            .min_elem()
                                            .min(1.0)
                                    } else {
                                        self.video_zoom.scale as f32
                                    };

                                    // If each pixel of the video is at least 2 physical pixels large, use nearest-
                                    // neighbor sampling. Otherwise, use linear sampling.
                                    let pixel_size = scale_factor
                                        * egui::vec2(par.0 / par.1, 1.0)
                                        * ui.ctx().pixels_per_point();
                                    let use_nearest = pixel_size.min_elem() >= 2.0;
                                    preview.set_partial(
                                        [0, 0],
                                        ColorImage::filled([0, 0], Color32::RED),
                                        TextureOptions {
                                            magnification: if use_nearest {
                                                egui::TextureFilter::Nearest
                                            } else {
                                                egui::TextureFilter::Linear
                                            },
                                            minification: egui::TextureFilter::Linear,
                                            ..Default::default()
                                        },
                                    );

                                    // We need to render the splitscreen bar in the same area as the image. The
                                    // Response returned from ui.image() fills the entire scroll area, so we need
                                    // to do the layout ourselves.
                                    let image = egui::Image::from_texture((
                                        preview.id(),
                                        texture_size * scale_factor,
                                    ));
                                    let (rect, _) = ui.allocate_exact_size(
                                        texture_size * scale_factor,
                                        egui::Sense::hover(),
                                    );
                                    // Avoid texture sampling at non-integer coordinates (causes jaggies)
                                    let rect = egui::Rect::from_points(&[
                                        rect.min.floor(),
                                        rect.max.floor(),
                                    ]);
                                    ui.put(rect, image);

                                    if self.effect_preview.mode == EffectPreviewMode::SplitScreen
                                        && ui
                                            .put(
                                                rect,
                                                SplitScreen::new(
                                                    &mut self.effect_preview.preview_rect,
                                                ),
                                            )
                                            .changed()
                                    {
                                        egui_sink.set_property(
                                            "preview_mode",
                                            Self::sink_preview_mode(&self.effect_preview),
                                        )
                                    }
                                },
                            );
                        });
                });
            });
    }

    fn show_credits_dialog(&mut self, ctx: &egui::Context) {
        egui::Window::new("About + Credits")
            .open(&mut self.credits_dialog_open)
            .default_width(400.0)
            .show(ctx, |ui| {
                const VERSION: &str = env!("CARGO_PKG_VERSION");
                ui.heading(format!("{} v{VERSION}", Self::APP_ID));

                ui.separator();

                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ui.label("by ");
                    ui.add(egui::Hyperlink::from_label_and_url(
                        "valadaptive",
                        "https://github.com/valadaptive/",
                    ));
                });

                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ui.label("...loosely based on ");
                    ui.add(egui::Hyperlink::from_label_and_url(
                        "JargeZ/ntscqt",
                        "https://github.com/JargeZ/ntscqt/",
                    ));
                });

                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ui.label("...which is a GUI for ");
                    ui.add(egui::Hyperlink::from_label_and_url(
                        "zhuker/ntsc",
                        "https://github.com/zhuker/ntsc/",
                    ));
                });

                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ui.label("...which is a port of ");
                    ui.add(egui::Hyperlink::from_label_and_url(
                        "joncampbell123/composite-video-simulator",
                        "https://github.com/joncampbell123/composite-video-simulator/",
                    ));
                });
            });
    }

    fn show_app(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        if self.credits_dialog_open {
            self.show_credits_dialog(ui.ctx());
        }

        if self.third_party_licenses_dialog_open {
            self.show_third_party_licenses_dialog(ui.ctx());
        }

        if self.license_dialog_open {
            self.show_license_dialog(ui.ctx());
        }

        self.update_dialog.show(ui.ctx());

        if self.image_sequence_dialog_queued_render_job.is_some() {
            let modal =
                egui::Modal::new(egui::Id::new("directory_not_empty")).show(ui.ctx(), |ui| {
                    ui.set_max_width(ui.input(|i| i.content_rect().width() - 24.0).min(400.0));
                    ui.set_min_width(200.0);
                    ui.heading("Output directory is not empty");
                    ui.label(
                    "You're rendering an image sequence into a directory that isn't empty. This \
                     will output many individual image files into that directory.",
                );
                    ui.separator();

                    egui::Sides::new().show(
                        ui,
                        |_| {},
                        |ui| {
                            if ui.button("OK").clicked() {
                                let job = self
                                    .image_sequence_dialog_queued_render_job
                                    .take()
                                    .unwrap()(self);
                                match job {
                                    Ok(job) => {
                                        self.render_jobs.push(job);
                                    }
                                    Err(e) => {
                                        self.handle_error(&e);
                                    }
                                }
                            } else if ui.button("Cancel").clicked() {
                                self.image_sequence_dialog_queued_render_job = None;
                            }
                        },
                    );
                });

            if modal.should_close() {
                self.image_sequence_dialog_queued_render_job = None;
            }
        }

        egui::Panel::top("menu_bar")
            .interact_height(ui)
            .show(ui, |ui| {
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.menu_button("File", |ui| {
                        if ui.button("Open").clicked() {
                            let file_dialog =
                                rfd::AsyncFileDialog::new().set_parent(frame).pick_file();
                            let ctx = ui.ctx().clone();
                            self.spawn(async move {
                                let handle = file_dialog.await;

                                Some(Box::new(move |app: &mut NtscApp| match handle {
                                    Some(handle) => app.load_video(&ctx, handle.into()),
                                    None => Ok(()),
                                }) as _)
                            });

                            ui.close();
                        }
                        if ui.button("Quit").clicked() {
                            ui.send_viewport_cmd(egui::ViewportCommand::Close);
                            ui.close();
                        }
                    });

                    ui.menu_button("Edit", |ui| {
                        if ui
                            .add_enabled(
                                self.undoer.has_undo(&self.effect_settings),
                                egui::Button::new("Undo"),
                            )
                            .clicked()
                        {
                            self.undo();
                            ui.close();
                        }
                        if ui
                            .add_enabled(
                                self.undoer.has_redo(&self.effect_settings),
                                egui::Button::new("Redo"),
                            )
                            .clicked()
                        {
                            self.redo();
                            ui.close();
                        }
                    });

                    ui.menu_button("View", |ui| {
                        ui.menu_button("Theme", |ui| {
                            let mut color_theme_changed = false;
                            let mut theme_preference = ui.ctx().options(|opt| opt.theme_preference);
                            color_theme_changed |= ui
                                .selectable_value(
                                    &mut theme_preference,
                                    egui::ThemePreference::System,
                                    "System",
                                )
                                .on_hover_text("Follow system color theme")
                                .changed();
                            color_theme_changed |= ui
                                .selectable_value(
                                    &mut theme_preference,
                                    egui::ThemePreference::Light,
                                    "Light",
                                )
                                .on_hover_text("Use light mode")
                                .changed();
                            color_theme_changed |= ui
                                .selectable_value(
                                    &mut theme_preference,
                                    egui::ThemePreference::Dark,
                                    "Dark",
                                )
                                .on_hover_text("Use dark mode")
                                .changed();

                            if color_theme_changed {
                                // Results in a bit of "theme tearing" since every widget rendered after this will use a
                                // different color scheme than those rendered before it. Not really noticeable in practice.
                                ui.ctx().set_theme(theme_preference);
                                ui.close();
                            }
                        });

                        ui.menu_button("Zoom", |ui| {
                            let mut zoom = ui.ctx().zoom_factor();
                            let mut changed = false;
                            const ZOOM_FACTORS: &[f32] = &[0.75, 0.9, 1.0, 1.25, 1.5, 1.75, 2.0];
                            for item_zoom_factor in ZOOM_FACTORS {
                                changed |= ui
                                    .selectable_value(
                                        &mut zoom,
                                        *item_zoom_factor,
                                        format!("{}%", item_zoom_factor * 100.0),
                                    )
                                    .changed();
                            }
                            if changed {
                                ui.ctx().set_zoom_factor(zoom);
                                ui.close()
                            }
                        });
                    });

                    ui.menu_button("Help", |ui| {
                        if ui.button("Online Documentation ⤴").clicked() {
                            ui.ctx().open_url(egui::OpenUrl::new_tab(
                                "https://ntsc.rs/docs/standalone-application/",
                            ));
                            ui.close();
                        }

                        if ui.button("License").clicked() {
                            self.license_dialog_open = true;
                            ui.close();
                        }

                        if ui.button("Third-Party Licenses").clicked() {
                            self.third_party_licenses_dialog_open = true;
                            ui.close();
                        }

                        if ui.button("About + Credits").clicked() {
                            self.credits_dialog_open = true;
                            ui.close();
                        }

                        if ui.button("Check for Updates...").clicked() {
                            self.update_dialog.open();
                            ui.close();
                        }
                    });

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        const VERSION: &str = env!("CARGO_PKG_VERSION");
                        ui.label(format!("{} v{VERSION}", Self::APP_ID));

                        let mut close_error = false;
                        if let Some(error) = self.last_error.borrow().as_ref() {
                            egui::Frame::NONE
                                .corner_radius(3)
                                .fill(ui.style().visuals.extreme_bg_color)
                                .show(ui, |ui| {
                                    if ui.button("OK").clicked() {
                                        close_error = true;
                                    }
                                    ui.label(error);
                                    ui.colored_label(egui::Color32::YELLOW, "⚠");
                                });
                        }
                        if close_error {
                            *self.last_error.borrow_mut() = None;
                        }
                    });
                });
            });

        egui::Panel::left("controls")
            .frame(egui::Frame::side_top_panel(ui.style()).inner_margin(0.0))
            .resizable(true)
            .default_size(425.0)
            .size_range(300.0..=800.0)
            .show(ui, |parent_ui| {
                let mut ui = parent_ui.new_child(UiBuilder::new());

                egui::Panel::top("left_tabs")
                    .interact_height(&mut ui)
                    .show(&mut ui, |ui| {
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            ui.selectable_value(
                                &mut self.left_panel_state,
                                LeftPanelState::EffectSettings,
                                "Effect",
                            );
                            ui.selectable_value(
                                &mut self.left_panel_state,
                                LeftPanelState::RenderSettings,
                                "Render",
                            );
                        });
                    });

                egui::CentralPanel::default()
                    .frame(egui::Frame::central_panel(ui.style()).inner_margin(0.0))
                    .show(&mut ui, |ui| match self.left_panel_state {
                        LeftPanelState::EffectSettings => {
                            self.show_effect_settings(ui, frame);
                        }
                        LeftPanelState::RenderSettings => {
                            self.show_render_settings(ui, frame);
                        }
                    });

                // Take up exactly as much space as the resizable panel thinks. There are some really annoying egui bugs
                // in this area that the maintainer repeatedly claims to have fixed
                // (https://github.com/ntsc-rs/ntsc-rs/issues/780, https://github.com/emilk/egui/pull/8198).
                parent_ui.advance_cursor_after_rect(parent_ui.available_rect_before_wrap());
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::side_top_panel(ui.style()).inner_margin(0.0))
            .show(ui, |ui| {
                self.show_video_pane(ui, frame);
            });
    }

    fn show_loading_screen(ui: &mut egui::Ui) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.centered_and_justified(|ui| {
                ui.add(egui::Spinner::new().size(128.0));
            });
        });
    }

    fn show_error_screen(ui: &mut egui::Ui, error: &ApplicationError) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.vertical(|ui| {
                ui.heading("An error occurred while loading");
                ui.label(error.to_string());
            });
        });
    }

    fn handle_keyboard_shortcuts(&mut self, ctx: &egui::Context) {
        // Seems to deadlock if we call undo() / redo() inside the ctx.input callback, probably due to Undoer accessing
        // context state from behind a mutex.
        let (should_undo, should_redo) = ctx.input(|input| {
            (
                // Note that we match command/ctrl *only*; otherwise Ctrl+Shift+Z would count as undo since Ctrl+Z is a subset of Ctrl+Shift+Z
                input.modifiers.command_only() && input.key_pressed(egui::Key::Z),
                (input.modifiers.command_only() && input.key_pressed(egui::Key::Y))
                    || (input
                        .modifiers
                        .matches_exact(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT)
                        && input.key_pressed(egui::Key::Z)),
            )
        });
        if should_undo {
            self.undo();
        } else if should_redo {
            self.redo();
        }
    }
}

impl eframe::App for NtscApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.tick();

        match self.gstreamer_init.check() {
            GstreamerInitState::Initializing(..) => {
                Self::show_loading_screen(ui);
                return;
            }
            GstreamerInitState::Initialized(Err(error)) => {
                Self::show_error_screen(ui, error);
                return;
            }
            _ => {}
        }

        let mut pipeline_error = None::<PipelineError>;
        if let Some(pipeline) = &self.pipeline {
            let state = pipeline.state.lock().unwrap();
            let state = &*state;
            match state {
                PipelineStatus::Loading => {}
                PipelineStatus::Loaded => {
                    let info = self.pipeline.as_ref().unwrap();
                    let mut at_eos = info.at_eos.lock().unwrap();
                    if *at_eos {
                        let _ = info.pipeline.set_state(gstreamer::State::Paused);
                        *at_eos = false;
                    }
                }
                PipelineStatus::Error(err) => {
                    pipeline_error = Some(err.clone());
                }
            };
        }

        if let Some(err) = pipeline_error {
            self.close_video(ui.ctx());
            self.handle_error(&err);
        }

        self.handle_keyboard_shortcuts(ui.ctx());

        self.show_app(ui, frame);

        self.undoer
            .feed_state(ui.input(|input| input.time), &self.effect_settings);

        ui.update_dnd_state();
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if let Ok(settings_json) = self.settings_list.to_json_string(&self.effect_settings) {
            storage.set_string("effect_settings", settings_json);
        }

        if let Ok(settings_json) = self
            .settings_list_easy
            .to_json_string(&self.easy_mode_settings)
        {
            storage.set_string("easy_mode_settings", settings_json);
        }

        storage.set_string("easy_mode_enabled", self.easy_mode_enabled.to_string());

        eframe::set_value(storage, "render_settings", &self.render_settings);
        eframe::set_value(storage, "scale_settings", &self.video_scale);
    }
}
