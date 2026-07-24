//! Builds the OpenFX plugin and bundles it according to the OpenFX specification, optionally taking care of producing
//! a universal binary for macOS.
//! For more information, see https://openfx.readthedocs.io/en/main/Reference/ofxPackaging.html.

use clap::builder::PathBufValueParser;

use crate::util::targets::{MACOS_AARCH64, MACOS_X86_64, Target};
use crate::util::{PathBufExt, StatusExt, workspace_dir};

use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

pub fn command() -> clap::Command {
    clap::Command::new("build-ofx-plugin")
        .about(
            "Builds and bundles the OpenFX plugin, which is then output to `crates/openfx/build`.",
        )
        .arg(
            clap::Arg::new("release")
                .long("release")
                .help("Build the plugin in release mode")
                .conflicts_with("debug")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            clap::Arg::new("debug")
                .long("debug")
                .help("Build the plugin in debug mode")
                .conflicts_with("release")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            clap::Arg::new("target")
                .long("target")
                .help("Set the target triple to compile for")
                .default_value(current_platform::CURRENT_PLATFORM),
        )
        .arg(
            clap::Arg::new("macos-universal")
                .long("macos-universal")
                .help("Build a macOS universal library (x86_64 and aarch64)")
                .action(clap::ArgAction::SetTrue)
                .conflicts_with("target"),
        )
        .arg(
            clap::Arg::new("destdir")
                .long("destdir")
                .help("The directory that the OpenFX plugin bundle will be output to")
                .value_parser(PathBufValueParser::new())
                .default_value(
                    workspace_dir()
                        .plus_iter(["crates", "openfx-plugin", "build"])
                        .as_os_str()
                        .to_owned(),
                ),
        )
}

/// Creates the contents of the Info.plist file for the bundle when building for macOS.
fn get_info_plist() -> plist::Value {
    let cargo_toml_path = workspace_dir().plus_iter(["crates", "openfx-plugin", "Cargo.toml"]);
    let manifest = cargo_toml::Manifest::from_path(cargo_toml_path).unwrap();
    let version = manifest.package().version();

    let mut info_plist_contents = plist::dictionary::Dictionary::new();
    info_plist_contents.insert(
        "CFBundleInfoDictionaryVersion".to_string(),
        plist::Value::from("6.0"),
    );
    info_plist_contents.insert(
        "CFBundleDevelopmentRegion".to_string(),
        plist::Value::from("en"),
    );
    info_plist_contents.insert(
        "CFBundlePackageType".to_string(),
        plist::Value::from("BNDL"),
    );
    info_plist_contents.insert(
        "CFBundleIdentifier".to_string(),
        plist::Value::from("rs.ntsc.openfx"),
    );
    info_plist_contents.insert(
        "CFBundleVersion".to_string(),
        plist::Value::from(version.to_string()),
    );
    info_plist_contents.insert(
        "CFBundleShortVersionString".to_string(),
        plist::Value::from(version.to_string()),
    );
    info_plist_contents.insert(
        "NSHumanReadableCopyright".to_string(),
        plist::Value::from("© 2023-2025 valadaptive"),
    );
    info_plist_contents.insert("CFBundleSignature".to_string(), plist::Value::from("????"));

    plist::Value::Dictionary(info_plist_contents)
}

/// Build the plugin for a given target, in either debug or release mode. This is called once in most cases, but when
/// creating a macOS universal binary, it's called twice--once per architecture.
/// This returns the path to the built library.
fn build_plugin_for_target(target: &Target, release_mode: bool) -> std::io::Result<PathBuf> {
    println!("Building OpenFX plugin for target {}", target.target_triple);

    let mut cargo_args: Vec<_> = vec![
        String::from("build"),
        String::from("--package=ntsc-rs-openfx-plugin"),
        String::from("--lib"),
        String::from("--target"),
        target.target_triple.to_string(),
    ];
    if release_mode {
        cargo_args.push(String::from("--release"));
    }
    Command::new("cargo")
        .args(&cargo_args)
        .status()
        .expect_success()?;

    let target_dir_path = workspace_dir().to_path_buf().plus_iter([
        "target",
        target.target_triple,
        if cargo_args.contains(&String::from("--release")) {
            "release"
        } else {
            "debug"
        },
    ]);

    let mut built_library_path =
        target_dir_path.plus(target.library_prefix.to_owned() + "ntsc_rs_openfx_plugin");
    built_library_path.set_extension(target.library_extension);

    Ok(built_library_path)
}

pub fn main(args: &clap::ArgMatches) -> Result<(), Box<dyn Error>> {
    let release_mode = args.get_flag("release");

    // TODO: remove previous built bundle?

    let (built_library_path, ofx_architecture) = if args.get_flag("macos-universal") {
        let x86_64_target = MACOS_X86_64;
        let aarch64_target = MACOS_AARCH64;
        let x86_64_path = build_plugin_for_target(x86_64_target, release_mode)?;
        let aarch64_path = build_plugin_for_target(aarch64_target, release_mode)?;

        let dst_path = std::env::temp_dir().plus(format!(
            "ntsc-rs-ofx-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        ));

        // Combine the x86_64 and aarch64 builds into one using `lipo`, and output to the temp file we created
        // above.
        // TODO: Create the directories beforehand, output into that with lipo, and just rename it afterwards?
        Command::new("lipo")
            .args(&[
                OsString::from("-create"),
                OsString::from("-output"),
                dst_path.clone().into(),
                x86_64_path.into(),
                aarch64_path.into(),
            ])
            .status()
            .expect_success()?;

        // Both targets should have ofx_architecture: "MacOS" since it's a universal binary. Some platforms have
        // different bundle directories depending on the architecture, but as of Apple Silicon, that's not done for
        // macOS:
        // https://openfx.readthedocs.io/en/main/Reference/ofxPackaging.html#macos-architectures-and-universal-binaries
        assert_eq!(
            x86_64_target.ofx_architecture,
            aarch64_target.ofx_architecture
        );
        (dst_path, x86_64_target.ofx_architecture)
    } else {
        let target_triple = args.get_one::<String>("target").unwrap();
        let target = Target::from_triple(target_triple).unwrap();

        (
            build_plugin_for_target(target, release_mode)?,
            target.ofx_architecture,
        )
    };

    let output_dir = args.get_one::<PathBuf>("destdir").unwrap();

    let plugin_bundle_path = output_dir.plus_iter(["NtscRs.ofx.bundle", "Contents"]);
    let plugin_bin_path = plugin_bundle_path.plus_iter([ofx_architecture, "NtscRs.ofx"]);
    let plugin_resources_path = plugin_bundle_path.plus_iter(["Resources"]);

    fs::create_dir_all(plugin_bin_path.parent().unwrap())?;
    fs::create_dir_all(&plugin_resources_path)?;
    fs::copy(built_library_path, plugin_bin_path)?;
    if ofx_architecture == "MacOS" {
        get_info_plist().to_file_xml(plugin_bundle_path.plus("Info.plist"))?;
        fs::copy(
            workspace_dir().plus_iter(["assets", "macos_icon_less_detail.png"]),
            plugin_resources_path.plus("wtf.vala.NtscRs.png"),
        )?;
    } else {
        fs::copy(
            workspace_dir().plus_iter(["assets", "icon.png"]),
            plugin_resources_path.plus("wtf.vala.NtscRs.png"),
        )?;
    }

    Ok(())
}
