//! Builds and bundles the standalone GUI as a macOS .app bundle.

use crate::util::targets::{MACOS_AARCH64, MACOS_X86_64};
use crate::util::{PathBufExt, StatusExt, build_gui_for_target, copy_recursive, workspace_dir};

use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use clap::builder::PathBufValueParser;

pub fn command() -> clap::Command {
    clap::Command::new("macos-bundle")
        .about("Builds the standalone app and creates a macOS application bundle.")
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
            clap::Arg::new("destdir")
                .long("destdir")
                .help("The directory that the app bundle will be output to")
                .value_parser(PathBufValueParser::new())
                .default_value(workspace_dir().plus("build").as_os_str().to_owned()),
        )
}

/// Use the `sips` utility built into macOS to resize an image (used for the application icon).
/// See https://ss64.com/mac/sips.html.
fn resize_image(
    src_path: impl AsRef<Path>,
    dst_path: impl AsRef<Path>,
    size: u32,
) -> std::io::Result<()> {
    let size_str = OsString::from(size.to_string());
    let args = [
        OsString::from("-z"),
        size_str.clone(),
        size_str.clone(),
        OsString::from(src_path.as_ref()),
        OsString::from("--out"),
        OsString::from(dst_path.as_ref()),
    ];
    Command::new("sips").args(args).status().expect_success()?;
    Ok(())
}

pub fn main(args: &clap::ArgMatches) -> Result<(), Box<dyn Error>> {
    let release_mode = args.get_flag("release");

    // Build x86_64 and aarch64 binaries.
    // TODO: unlike the other macOS xtasks, this doesn't yet support choosing the targets.
    println!("Building binaries...");
    let x86_64_dir = build_gui_for_target(MACOS_X86_64, release_mode)?;
    let aarch64_dir = build_gui_for_target(MACOS_AARCH64, release_mode)?;

    // Extract gui version from Cargo.toml.
    println!("Getting version for Info.plist and creating bundle directories...");
    let mut cargo_toml_path = workspace_dir().to_path_buf();
    cargo_toml_path.extend(["crates", "gui", "Cargo.toml"]);
    let gui_manifest = cargo_toml::Manifest::from_path(cargo_toml_path)?;
    let gui_version = gui_manifest.package().version();

    // Construct Info.plist and bundle structure.
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
        plist::Value::from("APPL"),
    );
    info_plist_contents.insert(
        "CFBundleIdentifier".to_string(),
        plist::Value::from("rs.ntsc.standalone"),
    );
    info_plist_contents.insert(
        "CFBundleExecutable".to_string(),
        plist::Value::from("ntsc-rs-standalone"),
    );
    info_plist_contents.insert(
        "CFBundleIconFile".to_string(),
        plist::Value::from("icon.icns"),
    );
    info_plist_contents.insert(
        "CFBundleDisplayName".to_string(),
        plist::Value::from("ntsc-rs"),
    );
    info_plist_contents.insert("CFBundleName".to_string(), plist::Value::from("ntsc-rs"));
    info_plist_contents.insert(
        "CFBundleVersion".to_string(),
        plist::Value::from(gui_version.to_string()),
    );
    info_plist_contents.insert(
        "CFBundleShortVersionString".to_string(),
        plist::Value::from(gui_version.to_string()),
    );
    info_plist_contents.insert(
        "NSHumanReadableCopyright".to_string(),
        plist::Value::from("© 2023-2025 valadaptive"),
    );
    info_plist_contents.insert("CFBundleSignature".to_string(), plist::Value::from("????"));

    let build_dir_path = args.get_one::<PathBuf>("destdir").unwrap();
    let app_dir_path = build_dir_path.plus("ntsc-rs.app");
    let iconset_dir_path = build_dir_path.plus("ntsc-rs.iconset");

    // Clean up the previous build. If there is no previous build, this will fail; that's OK.
    let _ = fs::remove_dir_all(&app_dir_path);
    let _ = fs::remove_dir_all(&iconset_dir_path);

    let contents_dir_path = app_dir_path.plus("Contents");
    fs::create_dir_all(&contents_dir_path)?;

    let macos_dir_path = contents_dir_path.plus("MacOS");
    fs::create_dir_all(&macos_dir_path)?;

    let resources_dir_path = contents_dir_path.plus("Resources");
    fs::create_dir_all(&resources_dir_path)?;

    plist::Value::Dictionary(info_plist_contents)
        .to_file_xml(contents_dir_path.plus("Info.plist"))?;

    let app_executables = ["ntsc-rs-standalone", "ntsc-rs-cli"];

    for binary_name in app_executables {
        println!("Creating universal binary ({binary_name})...");
        // Combine x86_64 and aarch64 binaries and place the result in the bundle.
        Command::new("lipo")
            .args(&[
                OsString::from("-create"),
                OsString::from("-output"),
                macos_dir_path.plus(binary_name).into(),
                x86_64_dir.plus(binary_name).into(),
                aarch64_dir.plus(binary_name).into(),
            ])
            .status()
            .expect_success()?;
    }

    // Copy gstreamer libraries into the bundle.
    println!("Copying gstreamer libraries...");
    let src_gst_path = PathBuf::from("/Library/Frameworks/GStreamer.framework/Versions/1.0");
    let dst_gst_path =
        contents_dir_path.plus_iter(["Frameworks", "GStreamer.framework", "Versions", "1.0"]);
    let src_lib_path = src_gst_path.plus("lib");
    let dst_lib_path = dst_gst_path.plus("lib");
    // We only want dylibs, not the static libs also present.
    copy_recursive(&src_lib_path, &dst_lib_path, |entry| {
        entry.path().extension() == Some(OsStr::new("dylib"))
    })?;

    let src_libexec_path = src_gst_path.plus("libexec");
    let dst_libexec_path = dst_gst_path.plus("libexec");
    copy_recursive(&src_libexec_path, &dst_libexec_path, |_| true)?;

    // Add gstreamer rpath to the executable, so it can load the gstreamer libraries. According to
    // https://gstreamer.freedesktop.org/documentation/deploying/mac-osx.html?gi-language=c#location-of-dependent-dynamic-libraries,
    // macOS doesn't locate libraries relative to the executable. That page's prescribed solution is a convoluted
    // `osxrelocator.py` script that I've seen several versions of floating around, but I just use `install_name_tool`
    // and it *seems* to work fine. GStreamer includes many binaries which the page says also need to be
    // `install_name_tool`'d but it seems they now perform that step themselves. They also mention that you need to set
    // some environment variables to pick up on binaries also distributed with GStreamer, but they have seemingly made
    // those paths relative on their end too.
    for binary_name in app_executables {
        println!("Adding gstreamer rpath ({binary_name})...");
        Command::new("install_name_tool")
            .args([
                OsString::from("-add_rpath"),
                OsString::from(
                    "@executable_path/../Frameworks/GStreamer.framework/Versions/1.0/lib",
                ),
                OsString::from(macos_dir_path.plus(binary_name)),
            ])
            .status()
            .expect_success()?;
    }

    // Create the iconset. Adapted from https://stackoverflow.com/a/20703594.

    // First, we resize the icons to all the sizes that Apple specifies:
    // https://developer.apple.com/design/human-interface-guidelines/app-icons#macOS-app-icon-sizes
    // Note that we actually have 2 icons: one for larger sizes, and one for smaller sizes where the thin lines on the
    // VHS label are removed.
    println!("Resizing icons...");
    let src_icon_folder_path = workspace_dir().plus("assets");
    let icon_lg_path = src_icon_folder_path.plus("macos_icon.png");
    let icon_sm_path = src_icon_folder_path.plus("macos_icon_less_detail.png");

    fs::create_dir_all(&iconset_dir_path)?;

    resize_image(&icon_sm_path, iconset_dir_path.plus("icon_16x16.png"), 16)?;
    let icon_32_path = iconset_dir_path.plus("icon_32x32.png");
    resize_image(&icon_sm_path, &icon_32_path, 32)?;
    fs::copy(&icon_32_path, iconset_dir_path.plus("icon_16x16@2x.png"))?;

    resize_image(
        &icon_sm_path,
        iconset_dir_path.plus("icon_128x128.png"),
        128,
    )?;
    let icon_256_path = iconset_dir_path.plus("icon_256x256.png");
    resize_image(&icon_lg_path, &icon_256_path, 256)?;
    fs::copy(&icon_256_path, iconset_dir_path.plus("icon_128x128@2x.png"))?;

    let icon_512_path = iconset_dir_path.plus("icon_512x512.png");
    resize_image(&icon_lg_path, &icon_512_path, 512)?;
    fs::copy(&icon_512_path, iconset_dir_path.plus("icon_256x256@2x.png"))?;

    resize_image(
        &icon_lg_path,
        iconset_dir_path.plus("icon_512x512@2x.png"),
        1024,
    )?;

    // Combine the iconset files into a single .icns.
    println!("Creating iconset...");
    Command::new("iconutil")
        .args([
            OsString::from("-c"),
            OsString::from("icns"),
            OsString::from("-o"),
            OsString::from(resources_dir_path.plus("icon.icns")),
            OsString::from(iconset_dir_path),
        ])
        .status()
        .expect_success()?;

    // TODO: code signing and notarization

    println!("Done!");

    Ok(())
}
