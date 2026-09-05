use std::ffi::OsStr;

#[rustfmt::skip]
fn main() {
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() || std::env::var_os("CARGO_CFG_TARGET_OS").as_deref() == Some(OsStr::new("macos")) {
        const PF_PLUG_IN_VERSION: u16 = 13;
        const PF_PLUG_IN_SUBVERS: u16 = 28;

        const EFFECT_VERSION_MAJOR: u32 = 1;
        const EFFECT_VERSION_MINOR: u32 = 5;
        const EFFECT_VERSION_PATCH: u32 = 5;
        use pipl::*;
        pipl::plugin_build(vec![
            Property::Kind(PIPLType::AEEffect),
            Property::Name("NTSC-rs"),
            Property::Category("Stylize"),

            #[cfg(target_os = "windows")]
            Property::CodeWin64X86("EffectMain"),
            #[cfg(target_os = "macos")]
            Property::CodeMacIntel64("EffectMain"),
            #[cfg(target_os = "macos")]
            Property::CodeMacARM64("EffectMain"),

            Property::AE_PiPL_Version { major: 2, minor: 0 },
            Property::AE_Effect_Spec_Version { major: PF_PLUG_IN_VERSION, minor: PF_PLUG_IN_SUBVERS },
            Property::AE_Effect_Version {
                version: EFFECT_VERSION_MAJOR,
                subversion: EFFECT_VERSION_MINOR,
                bugversion: EFFECT_VERSION_PATCH,
                stage: Stage::Develop,
                build: 1,
            },
            Property::AE_Effect_Info_Flags(0),
            Property::AE_Effect_Global_OutFlags(
                OutFlags::NonParamVary |
                OutFlags::DeepColorAware |
                OutFlags::SendUpdateParamsUI
            ),
            Property::AE_Effect_Global_OutFlags_2(
                OutFlags2::ParamGroupStartCollapsedFlag |
                OutFlags2::SupportsSmartRender |
                OutFlags2::FloatColorAware |
                OutFlags2::RevealsZeroAlpha |
                OutFlags2::SupportsThreadedRendering |
                OutFlags2::SupportsGetFlattenedSequenceData
            ),
            Property::AE_Effect_Match_Name("ntsc-rs"),
            Property::AE_Reserved_Info(8),
            Property::AE_Effect_Support_URL("https://ntsc.rs/docs/after-effects-plugin/"),
        ]);
        println!("cargo:rustc-env=EFFECT_VERSION_MAJOR={EFFECT_VERSION_MAJOR}");
        println!("cargo:rustc-env=EFFECT_VERSION_MINOR={EFFECT_VERSION_MINOR}");
        println!("cargo:rustc-env=EFFECT_VERSION_PATCH={EFFECT_VERSION_PATCH}");
        println!("cargo:rustc-cfg=with_premiere");
        println!("cargo:rustc-cfg=catch_panics");
    }
}
