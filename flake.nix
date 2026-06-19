{
  description = "Tauri Android Development Environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [
            (import rust-overlay)
          ];
          config = {
            allowUnfree = true;
            android_sdk.accept_license = true;
          };
        };

        platformVersion = "36";

        androidComposition = pkgs.androidenv.composeAndroidPackages {
          includeNDK = true;
          platformVersions = [ platformVersion ];
          abiVersions = [ "x86_64" "arm64-v8a" ];
          buildToolsVersions = [ "35.0.0" ];
          includeSystemImages = true;
          systemImageTypes = [ "google_apis" ];
        };

        emulatorScript = pkgs.androidenv.emulateApp {
          name = "emulate-tauri";
          platformVersion = platformVersion;
          abiVersion = "x86_64";
          systemImageType = "google_apis";
        };

        androidSdk = androidComposition.androidsdk;

        rustToolchain = pkgs.rust-bin.stable."1.93.0".default.override {
          extensions = [ "rust-src" "rust-analysis" "clippy" "rustfmt" "rust-analyzer" ];
          targets = [
            "aarch64-linux-android"
            "x86_64-unknown-linux-gnu"
            "armv7-linux-androideabi"
            "i686-linux-android"
            "x86_64-linux-android"
          ];
        };
      in
      {
        devShells.default = pkgs.mkShell {
          nativeBuildInputs = with pkgs; [
            openssl
            pkg-config
            wrapGAppsHook4
            cargo
            cargo-tauri
            xdg-utils
            bun
          ];

          buildInputs = with pkgs; [
            rustToolchain
            librsvg
            webkitgtk_4_1
            androidSdk
            emulatorScript
          ];

          shellHook = ''
            # Android and Java Paths
            export ANDROID_HOME="${androidSdk}/libexec/android-sdk"
            export NDK_HOME="$ANDROID_HOME/ndk-bundle"
            export JAVA_HOME="${pkgs.zulu.home}"
            export XDG_DATA_DIRS="$GSETTINGS_SCHEMAS_PATH"

            # Exports the android build tools to path
            export PATH="$ANDROID_HOME/build-tools/35.0.0:$PATH"

            # Disables the DMA-BUF renderer in webkit
            # It causes crashes/weird behaviour otherwise
            export WEBKIT_DISABLE_DMABUF_RENDERER=1

            # Exports Gradle System Options
            # Below does two important things:
            # 1. Stops gradle from trying to download its own tools
            # 2. Forces gradle to use the nix version of aapt2, so nix does not crash out
            export GRADLE_OPTS="-Dorg.gradle.project.android.sdk.channel=0 -Dorg.gradle.project.android.builder.sdkDownload=false -Dorg.gradle.project.android.aapt2FromMavenOverride=${androidSdk}/libexec/android-sdk/build-tools/35.0.0/aapt2"
          '';
        };
      }
    );
}
