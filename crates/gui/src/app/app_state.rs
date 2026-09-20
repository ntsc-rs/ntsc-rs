use std::thread::JoinHandle;

use eframe::egui::{Rect, pos2};
use serde::{Deserialize, Serialize};
use snafu::ResultExt;

use crate::gst_utils::{gstreamer_error::GstreamerError, ntsc_pipeline::VideoScale};

use super::error::{ApplicationError, GstreamerInitSnafu};

#[derive(Debug, Serialize, Deserialize)]
pub struct VideoZoom {
    pub scale: f64,
    pub fit: bool,
}

impl Default for VideoZoom {
    fn default() -> Self {
        Self {
            scale: 1.0,
            fit: true,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VideoScaleState {
    pub scale: VideoScale,
    pub enabled: bool,
}

impl Default for VideoScaleState {
    fn default() -> Self {
        Self {
            scale: Default::default(),
            enabled: true,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AudioVolume {
    pub gain: f64,
    // If the user drags the volume slider all the way to 0, we want to keep track of what it was before they did that
    // so we can reset the volume to it when they click the unmute button. This prevents e.g. the user setting the
    // volume to 25%, dragging it down to 0%, then clicking unmute and having it reset to some really loud default
    // value.
    pub gain_pre_mute: f64,
    pub mute: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
pub enum EffectPreviewMode {
    #[default]
    Enabled,
    Disabled,
    SplitScreen,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct EffectPreviewSettings {
    pub mode: EffectPreviewMode,
    pub preview_rect: Rect,
}

impl Default for EffectPreviewSettings {
    fn default() -> Self {
        Self {
            mode: Default::default(),
            preview_rect: Rect::from_min_max(pos2(0.0, 0.0), pos2(0.5, 1.0)),
        }
    }
}

impl Default for AudioVolume {
    fn default() -> Self {
        Self {
            gain: 1.0,
            gain_pre_mute: 1.0,
            mute: false,
        }
    }
}

#[derive(Default, PartialEq, Eq)]
pub enum LeftPanelState {
    #[default]
    EffectSettings,
    RenderSettings,
}

/// Used for the loading screen (and error screen if GStreamer fails to initialize). We initialize GStreamer on its own
/// thread, and return the result via a JoinHandle.
#[derive(Debug)]
pub enum GstreamerInitState {
    Initializing(Option<JoinHandle<Result<(), GstreamerError>>>),
    Initialized(Result<(), ApplicationError>),
}

impl GstreamerInitState {
    pub fn check(&mut self) -> &mut Self {
        if let Self::Initializing(handle) = self
            && handle.as_ref().is_some_and(|h| h.is_finished())
        {
            // In order to be able to "move" the error between enum variants, we need to be able to mem::take the
            // join handle.
            let res = handle
                .take()
                .unwrap()
                .join()
                .unwrap()
                .context(GstreamerInitSnafu);
            *self = Self::Initialized(res);
        }

        self
    }
}
