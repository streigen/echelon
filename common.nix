{
  nixpkgs,
  rust-overlay,
  system,
}:

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
  buildToolsVersion = "35.0.0";

  androidComposition = pkgs.androidenv.composeAndroidPackages {
    includeNDK = true;
    platformVersions = [ platformVersion ];
    abiVersions = [
      "x86_64"
      "arm64-v8a"
    ];
    buildToolsVersions = [ buildToolsVersion ];
    includeSystemImages = true;
    systemImageTypes = [ "google_apis" ];
  };

  androidSdk = androidComposition.androidsdk;

  rustToolchain = pkgs.rust-bin.stable."1.93.0".default.override {
    extensions = [
      "rust-src"
      "rust-analysis"
      "clippy"
      "rustfmt"
      "rust-analyzer"
    ];
    targets = [
      "aarch64-linux-android"
      "x86_64-unknown-linux-gnu"
      "armv7-linux-androideabi"
      "i686-linux-android"
      "x86_64-linux-android"
    ];
  };

  shellHook = ''
    # Android and Java Paths
    export ANDROID_HOME="${androidSdk}/libexec/android-sdk"
    export NDK_HOME="$ANDROID_HOME/ndk-bundle"
    export JAVA_HOME="${pkgs.zulu.home}"
    export XDG_DATA_DIRS="$GSETTINGS_SCHEMAS_PATH"

    # Exports the android build tools to path
    export PATH="$ANDROID_HOME/build-tools/${buildToolsVersion}:$PATH"
  '';
in
{
  inherit
    pkgs
    platformVersion
    buildToolsVersion
    androidSdk
    rustToolchain
    shellHook
    ;

  shell = pkgs.mkShell {
    nativeBuildInputs = with pkgs; [
      cargo
      xdg-utils
      bun
    ];

    buildInputs = with pkgs; [
      rustToolchain
      androidSdk
    ];

    inherit shellHook;
  };
}
