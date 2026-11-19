{
  description = "ntsc-rs: analog NTSC/PAL video effect";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachSystem [ "x86_64-linux" "aarch64-linux" ] (system:
      let
        pkgs = import nixpkgs { inherit system; };
        gst = pkgs.gst_all_1;

        nativeBuildInputs = with pkgs; [
          pkg-config
          makeWrapper
          rustc
          cargo
        ];

        buildInputs = [
          pkgs.openssl
          pkgs.glib
          pkgs.libxkbcommon
          pkgs.wayland
          pkgs.libx11
          pkgs.libxcursor
          pkgs.libxi
          pkgs.libxrandr
          pkgs.vulkan-loader
          pkgs.libGL
          gst.gstreamer
          gst.gst-plugins-base
          gst.gst-plugins-good
          gst.gst-plugins-bad
          gst.gst-plugins-ugly
          gst.gst-libav
        ];

        runtimeLibs = pkgs.lib.makeLibraryPath [
          pkgs.libxkbcommon
          pkgs.wayland
          pkgs.libx11
          pkgs.libxcursor
          pkgs.libxi
          pkgs.libxrandr
          pkgs.vulkan-loader
          pkgs.libGL
        ];

        gstPluginPath = pkgs.lib.makeSearchPath "lib/gstreamer-1.0" [
          gst.gstreamer
          gst.gst-plugins-base
          gst.gst-plugins-good
          gst.gst-plugins-bad
          gst.gst-plugins-ugly
          gst.gst-libav
        ];
      in
      {
        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "ntsc-rs";
          version = "0.9.7";
          src = pkgs.lib.cleanSource ./.;

          cargoLock = {
            lockFile = ./Cargo.lock;
            # Git dependencies (parley fork, after-effects) are pinned by rev in Cargo.lock.
            allowBuiltinFetchGit = true;
          };

          inherit nativeBuildInputs buildInputs;

          # Only build the GUI/CLI package; the AE plugin needs the Adobe SDK.
          cargoBuildFlags = [ "-p" "ntsc-rs-gui" ];
          cargoTestFlags = [ "-p" "ntsc-rs" ];

          postInstall = ''
            for bin in ntsc-rs-standalone ntsc-rs-launcher ntsc-rs-cli; do
              wrapProgram $out/bin/$bin \
                --prefix LD_LIBRARY_PATH : ${runtimeLibs} \
                --prefix GST_PLUGIN_SYSTEM_PATH_1_0 : ${gstPluginPath}
            done
          '';

          meta = {
            description = "Free, open-source analog NTSC/PAL video effect";
            homepage = "https://ntsc.rs";
            license = with pkgs.lib.licenses; [ mit asl20 isc ];
            mainProgram = "ntsc-rs-standalone";
            platforms = pkgs.lib.platforms.linux;
          };
        };

        devShells.default = pkgs.mkShell {
          inherit buildInputs;
          nativeBuildInputs = nativeBuildInputs ++ [ pkgs.rustfmt pkgs.clippy pkgs.rust-analyzer ];
          LD_LIBRARY_PATH = runtimeLibs;
          GST_PLUGIN_SYSTEM_PATH_1_0 = gstPluginPath;
        };
      });
}
