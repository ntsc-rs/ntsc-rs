//! Builds and bundles the standalone GUI for Windows (w/ GStreamer libraries copied over).

use clap::builder::PathBufValueParser;

use crate::util::targets::Target;
use crate::util::{PathBufExt, build_gui_for_target, copy_recursive, workspace_dir};

use std::error::Error;
use std::ffi::OsStr;
use std::fs;
use std::path::PathBuf;

pub fn command() -> clap::Command {
    clap::Command::new("windows-bundle")
        .about(
            "Builds and bundles the GUI on Windows, copying over GStreamer shared libraries and constructing the proper folder structure.",
        )
        .arg(
            clap::Arg::new("release")
                .long("release")
                .help("Build the software in release mode")
                .conflicts_with("debug")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            clap::Arg::new("debug")
                .long("debug")
                .help("Build the software in debug mode")
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
            clap::Arg::new("destdir")
                .long("destdir")
                .help("The directory that the app shortcut, libraries, etc. will be written into. This directory will be *cleared* on each run.")
                .value_parser(PathBufValueParser::new())
                .default_value(workspace_dir().plus_iter(["build", "ntsc-rs-windows-standalone"]).as_os_str().to_owned()),
        )
}

pub fn main(args: &clap::ArgMatches) -> Result<(), Box<dyn Error>> {
    let release_mode = args.get_flag("release");
    let build_dir_path = args.get_one::<PathBuf>("destdir").unwrap();
    let dst_bin_path = build_dir_path.plus("bin");

    // Clean up the previous build. If there is no previous build, this will fail; that's OK.
    let _ = fs::remove_dir_all(build_dir_path);
    fs::create_dir_all(&dst_bin_path)?;

    // Build the GUI first.
    let target_triple = args.get_one::<String>("target").unwrap();
    let target = Target::from_triple(target_triple)?;
    println!("Building binaries...");
    let bin_dir = build_gui_for_target(target, release_mode)?;
    bin_dir.plus("ntsc-rs-standalone.exe");
    &dst_bin_path;
    fs::copy(
        bin_dir.plus("ntsc-rs-standalone.exe"),
        dst_bin_path.plus("ntsc-rs-standalone.exe"),
    )
    .unwrap();
    fs::copy(
        bin_dir.plus("ntsc-rs-cli.exe"),
        dst_bin_path.plus("ntsc-rs-cli.exe"),
    )
    .unwrap();
    fs::copy(
        bin_dir.plus("ntsc-rs-launcher.exe"),
        build_dir_path.plus("ntsc-rs-launcher.exe"),
    )
    .unwrap();

    println!("Copying GStreamer libraries...");
    let gst_root = std::env::var_os("GSTREAMER_1_0_ROOT_MSVC_X86_64")
        .map(PathBuf::from)
        .ok_or("GSTREAMER_1_0_ROOT_MSVC_X86_64 not set (is GStreamer installed?)")?;
    copy_recursive(&gst_root, build_dir_path, |entry| {
        entry.path().extension() == Some(OsStr::new("dll"))
    })?;
    copy_recursive(
        gst_root.plus_iter(["share", "licenses"]),
        build_dir_path.plus("licenses"),
        |_| true,
    )?;

    Ok(())
}
