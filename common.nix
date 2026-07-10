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

  rustToolchain = pkgs.rust-bin.stable."1.96.1".default.override {
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

  runtimeLibs = with pkgs; [
    wayland
    libxkbcommon
    libGL
    vulkan-loader

    # X11 fallbacks (optional but highly recommended)
    libx11
    libxcursor
    libxi
    libxrandr
  ];

  shellHook = ''
    # Android and Java Paths
    export ANDROID_HOME="${androidSdk}/libexec/android-sdk"
    export NDK_HOME="$ANDROID_HOME/ndk-bundle"
    export JAVA_HOME="${pkgs.zulu.home}"
    export XDG_DATA_DIRS="$GSETTINGS_SCHEMAS_PATH"

    export RUSTC_WRAPPER="sccache"

    # Exports the android build tools to path
    export PATH="$ANDROID_HOME/build-tools/${buildToolsVersion}:$PATH"
    export LD_LIBRARY_PATH="/run/opengl-driver/lib:/run/opengl-driver-32/lib:${pkgs.lib.makeLibraryPath runtimeLibs}:$LD_LIBRARY_PATH"
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
      rustToolchain
      xdg-utils
      pkg-config
      mold
      clang
      sccache
      upx
      perf
    ];

    buildInputs =
      with pkgs;
      [
        fontconfig
        androidSdk
      ]
      ++ runtimeLibs;

    inherit shellHook;
  };
}
