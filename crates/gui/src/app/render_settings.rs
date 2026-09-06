use std::{ops::RangeInclusive, path::PathBuf};

use gstreamer::{ClockTime, Fraction};
use ntsc_rs::NtscEffect;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct H264Settings {
    /// Quality (the inverse of the constant rate factor). Although libx264 says it ranges from 0-51, its actual range
    /// appears to be 0-50 inclusive. It's flipped from CRF so 50 is lossless and 0 is worst.
    pub quality: u8,
    /// 0-8 for libx264 presets veryslow-ultrafast
    pub encode_speed: u8,
    /// Enable 10-bit color
    pub ten_bit: bool,
    /// Subsample chroma to 4:2:0
    pub chroma_subsampling: bool,
}

impl H264Settings {
    pub const QUALITY_RANGE: RangeInclusive<u8> = 0..=50;
    pub const ENCODE_SPEED_RANGE: RangeInclusive<u8> = 0..=8;
}

impl Default for H264Settings {
    fn default() -> Self {
        Self {
            quality: 27,
            encode_speed: 5,
            ten_bit: false,
            chroma_subsampling: true,
        }
    }
}

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Ffv1BitDepth {
    #[default]
    Bits8,
    Bits10,
    Bits12,
}

impl Ffv1BitDepth {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Bits8 => "8-bit",
            Self::Bits10 => "10-bit",
            Self::Bits12 => "12-bit",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Ffv1Settings {
    pub bit_depth: Ffv1BitDepth,
    // Subsample chroma to 4:2:0
    pub chroma_subsampling: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PngSequenceSettings {
    // TODO: bit depth (requires 16bpc RGB support in GStreamer and reimplementing the PNG encoder plugin)
    pub compression_level: u8,
}

impl PngSequenceSettings {
    pub const COMPRESSION_LEVEL_RANGE: RangeInclusive<u8> = 0..=9;
}

impl Default for PngSequenceSettings {
    fn default() -> Self {
        Self {
            compression_level: 6,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PngSettings {
    pub seek_to: ClockTime,
    pub settings: PngSequenceSettings,
}

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputCodec {
    #[default]
    H264,
    Ffv1,
    PngSequence,
}

impl OutputCodec {
    pub fn label(&self) -> &'static str {
        match self {
            Self::H264 => "H.264",
            Self::Ffv1 => "FFV1 (Lossless)",
            Self::PngSequence => "PNG Sequence",
        }
    }

    pub fn extension(&self) -> &'static str {
        match self {
            Self::H264 => "mp4",
            Self::Ffv1 => "mkv",
            Self::PngSequence => "png",
        }
    }

    pub fn is_image_sequence(&self) -> bool {
        *self == Self::PngSequence
    }
}

#[derive(Debug, Clone)]
pub enum RenderPipelineCodec {
    H264(H264Settings),
    Ffv1(Ffv1Settings),
    Png(PngSettings),
    PngSequence(PngSequenceSettings),
}

impl RenderPipelineCodec {
    pub fn is_image_sequence(&self) -> bool {
        matches!(&self, Self::Png(_) | Self::PngSequence(_))
    }
}

#[derive(Debug, Clone)]
pub struct StillImageSettings {
    pub framerate: Fraction,
    pub duration: ClockTime,
}

#[derive(Debug, Clone)]
pub struct RenderPipelineSettings {
    pub codec_settings: RenderPipelineCodec,
    pub output_path: PathBuf,
    pub interlaced_output: bool,
    pub effect_settings: NtscEffect,
}

impl RenderPipelineSettings {
    pub fn from_gui_settings(
        effect_settings: &NtscEffect,
        render_settings: &RenderSettings,
    ) -> Self {
        Self {
            codec_settings: render_settings.into(),
            output_path: render_settings.output_path.clone(),
            interlaced_output: render_settings.interlaced
                && render_settings.interlaced_output_allowed(),
            effect_settings: effect_settings.clone(),
        }
    }
}

#[derive(Default, Debug, Clone, Serialize, Deserialize)]
pub struct RenderSettings {
    pub output_codec: OutputCodec,
    // we want to keep these around even if the user changes their mind and selects a different codec, so they don't
    // lose the settings if they change back
    pub h264_settings: H264Settings,
    pub ffv1_settings: Ffv1Settings,
    pub png_sequence_settings: PngSequenceSettings,
    #[serde(skip)]
    pub output_path: PathBuf,
    pub duration: ClockTime,
    pub interlaced: bool,
}

impl RenderSettings {
    pub fn interlaced_output_allowed(&self) -> bool {
        !self.output_codec.is_image_sequence()
    }
}

impl From<&RenderSettings> for RenderPipelineCodec {
    fn from(value: &RenderSettings) -> Self {
        match value.output_codec {
            OutputCodec::H264 => RenderPipelineCodec::H264(value.h264_settings.clone()),
            OutputCodec::Ffv1 => RenderPipelineCodec::Ffv1(value.ffv1_settings.clone()),
            OutputCodec::PngSequence => {
                RenderPipelineCodec::PngSequence(value.png_sequence_settings.clone())
            }
        }
    }
}
