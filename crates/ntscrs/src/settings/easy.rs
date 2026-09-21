use alloc::{boxed::Box, vec};

#[cfg(not(feature = "std"))]
use core_maths::CoreFloat as _;

use super::{
    MenuItem, SettingDescriptor, SettingKind, Settings, SettingsBlock,
    standard::{
        ChromaDemodulationFilter, ChromaLowpass, FbmNoiseSettings, FilterType,
        HeadSwitchingMidLineSettings, HeadSwitchingSettings, LumaLowpass, NtscEffect,
        RingingSettings, ScaleSettings, TrackingNoiseSettings, UseField, VHSEdgeWaveSettings,
        VHSSettings, VHSSharpenSettings, VHSTapeSpeed,
    },
};

#[derive(Clone, Debug, PartialEq)]
pub struct EzTrackingNoiseSettings {
    height: i32,
    intensity: f32,
}

impl Default for EzTrackingNoiseSettings {
    fn default() -> Self {
        Self {
            height: 12,
            intensity: 0.5,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct EzVHSSettings {
    tape_speed: VHSTapeSpeed,
    chroma_loss: f32,
    sharpen: f32,
    edge_wave: f32,
    head_switching: f32,
    tracking_noise: SettingsBlock<EzTrackingNoiseSettings>,
}

impl Default for EzVHSSettings {
    fn default() -> Self {
        Self {
            tape_speed: VHSTapeSpeed::SP,
            chroma_loss: 0.0,
            sharpen: 0.0,
            edge_wave: 0.5,
            head_switching: 6.0,
            tracking_noise: Default::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct EasyMode {
    random_seed: i32,
    use_field: UseField,
    filter_type: FilterType,
    saturation: f32,
    snow: f32,
    chroma_demodulation_filter: ChromaDemodulationFilter,
    luma_smear: f32,
    ringing: f32,
    luma_noise: SettingsBlock<FbmNoiseSettings>,
    chroma_noise: SettingsBlock<FbmNoiseSettings>,
    chroma_phase_noise: f32,
    vhs_settings: SettingsBlock<EzVHSSettings>,
}

impl Default for EasyMode {
    fn default() -> Self {
        Self {
            random_seed: 0,
            use_field: UseField::InterleavedUpper,
            filter_type: FilterType::Butterworth,
            saturation: 0.25,
            snow: 0.0005,
            chroma_demodulation_filter: ChromaDemodulationFilter::Notch,
            luma_smear: 0.2,
            ringing: 0.5,
            luma_noise: SettingsBlock::enabled(FbmNoiseSettings {
                frequency: 0.1,
                intensity: 0.01,
                detail: 2,
            }),
            chroma_noise: SettingsBlock::enabled(FbmNoiseSettings {
                frequency: 0.05,
                intensity: 0.1,
                detail: 2,
            }),
            chroma_phase_noise: 0.01,
            vhs_settings: Default::default(),
        }
    }
}

#[rustfmt::skip]
pub mod setting_id {
    use crate::{setting_id, settings::{SettingID, easy::EasyMode}};
    type EasySettingID = SettingID<EasyMode>;

    pub const RANDOM_SEED: EasySettingID = setting_id!(4096 | 0, "ez_random_seed", random_seed);
    pub const USE_FIELD: EasySettingID = setting_id!(4096 | 1, "ez_use_field", use_field);
    pub const FILTER_TYPE: EasySettingID = setting_id!(4096 | 2, "ez_filter_type", filter_type);
    pub const SATURATION: EasySettingID = setting_id!(4096 | 3, "ez_saturation", saturation);
    pub const SNOW: EasySettingID = setting_id!(4096 | 4, "ez_snow", snow);
    pub const CHROMA_DEMODULATION_FILTER: EasySettingID = setting_id!(4096 | 5, "ez_chroma_demodulation_filter", chroma_demodulation_filter);
    pub const LUMA_SMEAR: EasySettingID = setting_id!(4096 | 6, "ez_luma_smear", luma_smear);
    pub const RINGING: EasySettingID = setting_id!(4096 | 7, "ez_ringing", ringing);
    pub const VHS_SETTINGS: EasySettingID = setting_id!(4096 | 8, "ez_vhs_settings", vhs_settings.enabled);
    pub const VHS_HEAD_SWITCHING: EasySettingID = setting_id!(4096 | 9, "ez_vhs_head_switching", vhs_settings.settings.head_switching);
    pub const TRACKING_NOISE_ENABLED: EasySettingID = setting_id!(4096 | 10, "ez_tracking_noise_enabled", vhs_settings.settings.tracking_noise.enabled);
    pub const TRACKING_NOISE_HEIGHT: EasySettingID = setting_id!(4096 | 11, "ez_tracking_noise_height", vhs_settings.settings.tracking_noise.settings.height);
    pub const TRACKING_NOISE_INTENSITY: EasySettingID = setting_id!(4096 | 12, "ez_tracking_noise_intensity", vhs_settings.settings.tracking_noise.settings.intensity);

    pub const LUMA_NOISE_ENABLED: EasySettingID = setting_id!(4096 | 13, "ez_luma_noise_enabled", luma_noise.enabled);
    pub const LUMA_NOISE_FREQUENCY: EasySettingID = setting_id!(4096 | 14, "ez_luma_noise_frequency", luma_noise.settings.frequency);
    pub const LUMA_NOISE_INTENSITY: EasySettingID = setting_id!(4096 | 15, "ez_luma_noise_intensity", luma_noise.settings.intensity);
    pub const LUMA_NOISE_DETAIL: EasySettingID = setting_id!(4096 | 16, "ez_luma_noise_detail", luma_noise.settings.detail);

    pub const CHROMA_NOISE_ENABLED: EasySettingID = setting_id!(4096 | 17, "ez_chroma_noise_enabled", chroma_noise.enabled);
    pub const CHROMA_NOISE_FREQUENCY: EasySettingID = setting_id!(4096 | 18, "ez_chroma_noise_frequency", chroma_noise.settings.frequency);
    pub const CHROMA_NOISE_INTENSITY: EasySettingID = setting_id!(4096 | 19, "ez_chroma_noise_intensity", chroma_noise.settings.intensity);
    pub const CHROMA_NOISE_DETAIL: EasySettingID = setting_id!(4096 | 20, "ez_chroma_noise_detail", chroma_noise.settings.detail);

    pub const CHROMA_PHASE_NOISE: EasySettingID = setting_id!(4096 | 21, "ez_chroma_phase_noise", chroma_phase_noise);
    pub const VHS_TAPE_SPEED: EasySettingID = setting_id!(4096 | 22, "ez_vhs_tape_speed", vhs_settings.settings.tape_speed);
    pub const VHS_CHROMA_LOSS: EasySettingID = setting_id!(4096 | 22, "ez_vhs_chroma_loss", vhs_settings.settings.chroma_loss);
    pub const VHS_SHARPEN: EasySettingID = setting_id!(4096 | 23, "ez_vhs_sharpen", vhs_settings.settings.sharpen);
    pub const VHS_EDGE_WAVE: EasySettingID = setting_id!(4096 | 24, "ez_vhs_edge_wave", vhs_settings.settings.edge_wave);
}

impl Settings for EasyMode {
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
                label: "Saturation",
                description: Some(
                    "Boost high frequencies in the NTSC signal, sharpening the image and \
                     intensifying colors.",
                ),
                kind: SettingKind::FloatRange {
                    range: -1.0..=2.0,
                    logarithmic: false,
                },
                id: setting_id::SATURATION,
            },
            SettingDescriptor {
                label: "Snow",
                description: Some("Frequency of random speckles in the image."),
                kind: SettingKind::FloatRange {
                    range: 0.0..=100.0,
                    logarithmic: true,
                },
                id: setting_id::SNOW,
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
                            label: "1-line comb",
                            description: Some(
                                "Average the current row with the previous one, phase-cancelling \
                                 the chrominance signals. Only works if the scanline phase shift \
                                 is 180 degrees.",
                            ),
                            index: ChromaDemodulationFilter::OneLineComb as u32,
                        },
                        MenuItem {
                            label: "2-line comb",
                            description: Some(
                                "Average the current row with the previous and next ones, \
                                 phase-cancelling the chrominance signals. Only works if the \
                                 scanline phase shift is 180 degrees.",
                            ),
                            index: ChromaDemodulationFilter::TwoLineComb as u32,
                        },
                    ],
                },
                id: setting_id::CHROMA_DEMODULATION_FILTER,
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
                label: "Ringing",
                description: Some("Additional ringing artifacts, simulated with a notch filter."),
                kind: SettingKind::Percentage { logarithmic: false },
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
                id: setting_id::LUMA_NOISE_ENABLED,
            },
            SettingDescriptor {
                label: "Chroma noise",
                description: Some("Noise applied to the chrominance signal."),
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
                id: setting_id::CHROMA_NOISE_ENABLED,
            },
            SettingDescriptor {
                label: "Chroma phase noise",
                description: Some(
                    "Noise applied per-scanline to the phase of the chrominance signal.",
                ),
                kind: SettingKind::Percentage { logarithmic: false },
                id: setting_id::CHROMA_PHASE_NOISE,
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
                            label: "Head switching",
                            description: Some(
                                "Emulate VHS head-switching artifacts at the bottom of the image.",
                            ),
                            kind: SettingKind::FloatRange {
                                range: 0.0..=24.0,
                                logarithmic: false,
                            },
                            id: setting_id::VHS_HEAD_SWITCHING,
                        },
                        SettingDescriptor {
                            label: "Tracking noise",
                            description: Some("Emulate noise from VHS tracking error."),
                            kind: SettingKind::Group {
                                children: vec![
                                    SettingDescriptor {
                                        label: "Height",
                                        description: None,
                                        kind: SettingKind::IntRange { range: 0..=120 },
                                        id: setting_id::TRACKING_NOISE_HEIGHT,
                                    },
                                    SettingDescriptor {
                                        label: "Intensity",
                                        description: None,
                                        kind: SettingKind::Percentage { logarithmic: false },
                                        id: setting_id::TRACKING_NOISE_INTENSITY,
                                    },
                                ],
                            },
                            id: setting_id::TRACKING_NOISE_ENABLED,
                        },
                        SettingDescriptor {
                            label: "Chroma loss",
                            description: Some(
                                "Chance that the chrominance signal is completely lost in each \
                                 scanline.",
                            ),
                            kind: SettingKind::Percentage { logarithmic: true },
                            id: setting_id::VHS_CHROMA_LOSS,
                        },
                        SettingDescriptor {
                            label: "Sharpen",
                            description: Some(
                                "Sharpening of the image, as done by some VHS decks.",
                            ),
                            kind: SettingKind::Percentage { logarithmic: false },
                            id: setting_id::VHS_SHARPEN,
                        },
                        SettingDescriptor {
                            label: "Edge wave",
                            description: Some("Horizontal waving of the image."),
                            kind: SettingKind::FloatRange {
                                range: 0.0..=20.0,
                                logarithmic: false,
                            },
                            id: setting_id::VHS_EDGE_WAVE,
                        },
                    ],
                },
                id: setting_id::VHS_SETTINGS,
            },
        ]
        .into_boxed_slice()
    }

    fn legacy_value() -> Self {
        Default::default() // this will remain this way until (unless) easy mode is ever finished
    }
}

impl From<&EasyMode> for NtscEffect {
    fn from(easy_settings: &EasyMode) -> Self {
        Self {
            random_seed: easy_settings.random_seed,
            use_field: easy_settings.use_field,
            filter_type: easy_settings.filter_type,
            input_luma_filter: LumaLowpass::Notch,
            chroma_lowpass_in: ChromaLowpass::None,
            chroma_demodulation: easy_settings.chroma_demodulation_filter,
            luma_smear: easy_settings.luma_smear,
            composite_sharpening: easy_settings.saturation,
            head_switching: SettingsBlock {
                enabled: easy_settings.vhs_settings.enabled
                    && easy_settings.vhs_settings.settings.head_switching > 0.0,
                settings: HeadSwitchingSettings {
                    height: (easy_settings.vhs_settings.settings.head_switching * 1.25).round()
                        as i32,
                    offset: (easy_settings.vhs_settings.settings.head_switching * 0.25).round()
                        as i32,
                    horiz_shift: easy_settings.vhs_settings.settings.head_switching * 8.0,
                    mid_line: SettingsBlock {
                        enabled: true,
                        settings: HeadSwitchingMidLineSettings {
                            position: 0.8,
                            jitter: 0.1,
                        },
                    },
                },
            },
            tracking_noise: SettingsBlock {
                enabled: easy_settings.vhs_settings.enabled
                    && easy_settings.vhs_settings.settings.tracking_noise.enabled,
                settings: TrackingNoiseSettings {
                    height: easy_settings
                        .vhs_settings
                        .settings
                        .tracking_noise
                        .settings
                        .height,
                    wave_intensity: easy_settings
                        .vhs_settings
                        .settings
                        .tracking_noise
                        .settings
                        .intensity
                        * 5.0,
                    snow_intensity: easy_settings
                        .vhs_settings
                        .settings
                        .tracking_noise
                        .settings
                        .intensity
                        * 0.5,
                    snow_anisotropy: 0.25,
                    noise_intensity: easy_settings
                        .vhs_settings
                        .settings
                        .tracking_noise
                        .settings
                        .intensity
                        * 0.5,
                },
            },
            composite_noise: SettingsBlock {
                enabled: false,
                settings: NtscEffect::default().composite_noise.settings,
            },
            ringing: SettingsBlock {
                enabled: easy_settings.ringing > 0.0,
                settings: RingingSettings {
                    frequency: 0.4,
                    power: 2.0,
                    intensity: easy_settings.ringing,
                },
            },
            luma_noise: easy_settings.luma_noise.clone(),
            chroma_noise: easy_settings.chroma_noise.clone(),
            snow_intensity: easy_settings.snow,
            snow_anisotropy: 0.5,
            chroma_phase_noise_intensity: easy_settings.chroma_phase_noise * 0.5,
            chroma_phase_error: 0.0,
            chroma_delay_horizontal: 0.0,
            chroma_delay_vertical: 0,
            vhs_settings: SettingsBlock {
                enabled: easy_settings.vhs_settings.enabled,
                settings: VHSSettings {
                    tape_speed: easy_settings.vhs_settings.settings.tape_speed,
                    chroma_loss: easy_settings.vhs_settings.settings.chroma_loss,
                    sharpen: SettingsBlock {
                        enabled: easy_settings.vhs_settings.settings.sharpen > 0.0,
                        settings: VHSSharpenSettings {
                            intensity: easy_settings.vhs_settings.settings.sharpen,
                            frequency: 0.5,
                        },
                    },
                    edge_wave: SettingsBlock {
                        enabled: easy_settings.vhs_settings.settings.edge_wave > 0.0,
                        settings: VHSEdgeWaveSettings {
                            intensity: easy_settings.vhs_settings.settings.edge_wave,
                            speed: 7.0,
                            frequency: 0.1,
                            detail: 2,
                        },
                    },
                },
            },
            chroma_vert_blend: !easy_settings.vhs_settings.enabled,
            chroma_lowpass_out: ChromaLowpass::Full,
            scale: SettingsBlock {
                enabled: false,
                settings: ScaleSettings {
                    horizontal_scale: 1.0,
                    vertical_scale: 1.0,
                    scale_with_video_size: false,
                },
            },
        }
    }
}
