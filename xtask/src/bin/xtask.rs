//! cargo-xtask is a pattern which provides a platform-independent way to run build scripts by writing them in Rust.
//! While many of the build scripts are to some degree platform-specific, there's a lot of shared logic that is nice
//! to be able to reuse between platforms.
//! See https://github.com/matklad/cargo-xtask for more information.

use std::process;

use xtask::{build_ofx_plugin, macos_ae_plugin, macos_bundle, windows_bundle};

fn main() {
    let cmd = clap::Command::new("xtask")
        .subcommand_required(true)
        .subcommand(build_ofx_plugin::command())
        .subcommand(macos_ae_plugin::command())
        .subcommand(macos_bundle::command())
        .subcommand(windows_bundle::command());

    let matches = cmd.get_matches();

    let (task, args) = matches.subcommand().unwrap();

    match task {
        "macos-ae-plugin" => {
            macos_ae_plugin::main(args).unwrap();
        }
        "macos-bundle" => {
            macos_bundle::main(args).unwrap();
        }
        "build-ofx-plugin" => {
            build_ofx_plugin::main(args).unwrap();
        }
        "windows-bundle" => {
            windows_bundle::main(args).unwrap();
        }
        _ => {
            println!("Invalid xtask: {task}");
            process::exit(1);
        }
    }
}
