<p align="center">
    <a href="https://ntsc.rs">
        <picture>
            <source media="(prefers-color-scheme: dark)" srcset="./docs/img/logo-darkmode.svg">
            <img alt="ntsc-rs logo" src="./docs/img/logo-lightmode.svg" width="1216">
        </picture>
    </a>
</p>

---

**ntsc-rs** is a video effect which emulates NTSC and VHS video artifacts. It can be used as an After Effects, Premiere, or OpenFX plugin, or as a standalone application.

![Screenshot of the ntsc-rs standalone application](./docs/img/appdemo.png)

## Download and Install

The latest version of ntsc-rs can be downloaded from [the releases page](https://github.com/valadaptive/ntsc-rs/releases).

After downloading, [read the documentation for how to run it](https://ntsc.rs/docs/standalone-installation/). In particular, ntsc-rs will not work properly on Linux unless you install all of the GStreamer packages listed in the documentation.

## More information

ntsc-rs is a rough Rust port of [ntscqt](https://github.com/JargeZ/ntscqt), a PyQt-based GUI for [ntsc](https://github.com/zhuker/ntsc), itself a Python port of [composite-video-simulator](https://github.com/joncampbell123/composite-video-simulator). Reimplementing the image processing in multithreaded Rust allows it to run at (mostly) real-time speeds.

It's not an exact port--some processing passes have visibly different results, and some new ones have been added.

## Building on Linux / Nix

The GUI needs GStreamer (base/good/bad/ugly/libav), OpenSSL, pkg-config, and the usual windowing/graphics libs.

- **Nix:** `nix run github:valadaptive/ntsc-rs` (or `nix run .`), `nix build`, or `nix develop` for a dev shell.
- **Debian/Ubuntu:** `sudo apt install build-essential pkg-config libssl-dev libglib2.0-dev libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev gstreamer1.0-plugins-{good,bad,ugly} gstreamer1.0-libav libxkbcommon-dev libwayland-dev libx11-dev libvulkan-dev`
- **Fedora:** `sudo dnf install gcc pkgconf-pkg-config openssl-devel glib2-devel gstreamer1-devel gstreamer1-plugins-base-devel gstreamer1-plugins-{good,bad-free,ugly} gstreamer1-libav libxkbcommon-devel wayland-devel libX11-devel vulkan-loader-devel`
- **Arch:** `sudo pacman -S base-devel pkgconf openssl gstreamer gst-plugins-{base,good,bad,ugly} gst-libav libxkbcommon wayland libx11 vulkan-icd-loader`

Then `cargo build --release -p ntsc-rs-gui`.
