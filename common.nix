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
  buildToolsVersion = "36.0.0";
  minSdkVersion = "26";

  androidComposition = pkgs.androidenv.composeAndroidPackages {
    includeNDK = true;
    includeEmulator = true;
    platformVersions = [
      platformVersion
      minSdkVersion
    ];
    abiVersions = [
      "x86_64"
      "arm64-v8a"
    ];
    buildToolsVersions = [ buildToolsVersion ];
    includeSystemImages = true;
    systemImageTypes = [ "default" ];
  };

  androidSdk = androidComposition.androidsdk;

  android_latest = pkgs.writeShellScriptBin "android_emulator_latest" ''
    exec "$ANDROID_HOME/emulator/emulator" -avd android_latest -no-boot-anim "$@"
  '';

  android_minimum = pkgs.writeShellScriptBin "android_emulator_minimum" ''
    exec "$ANDROID_HOME/emulator/emulator" -avd android_minimum -no-boot-anim "$@"
  '';

  build_android_apk = pkgs.writeShellScriptBin "android_build" ''
    exec bash ./scripts/android-build.sh "$@"
  '';

  # aapt2 from the Android SDK is a dynamically linked glibc ELF and can't run on
  # NixOS without nix-ld. Patch it so it works on any NixOS machine.
  aapt2Patched = pkgs.stdenv.mkDerivation {
    name = "aapt2-patched";
    nativeBuildInputs = [ pkgs.autoPatchelfHook ];
    buildInputs = [
      pkgs.zlib
      pkgs.stdenv.cc.cc.lib
    ];
    phases = [
      "installPhase"
      "fixupPhase"
    ];
    installPhase = ''
      mkdir -p $out/bin
      cp ${androidSdk}/libexec/android-sdk/build-tools/${buildToolsVersion}/aapt2 $out/bin/aapt2
      chmod +x $out/bin/aapt2
    '';
  };

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

    # no more "IT WORKS ON MY PC!!!!" i hope, lol
    unset NIX_LD_LIBRARY_PATH
    unset NIX_LD

    # Android and Java Paths
    export ANDROID_HOME="${androidSdk}/libexec/android-sdk"
    export ANDROID_SDK_HOME="$HOME/.android"
    export ANDROID_AVD_HOME="$HOME/.android/avd"
    mkdir -p "$ANDROID_AVD_HOME"
    export NDK_HOME="$ANDROID_HOME/ndk-bundle"
    export ANDROID_NDK_HOME="$NDK_HOME"
    export ANDROID_PLATFORM="android-${platformVersion}"
    export ANDROID_JAR="$ANDROID_HOME/platforms/android-${platformVersion}/android.jar"
    export JAVA_HOME="${pkgs.zulu.home}"
    export XDG_DATA_DIRS="$GSETTINGS_SCHEMAS_PATH"

    export RUSTC_WRAPPER="sccache"

    # Use nixpkgs clang (NixOS-native) with NDK sysroot for Android C/C++ cross-compilation.
    # NDK's own prebuilt clang is a glibc ELF that can't execute on NixOS without nix-ld.
    NDK_SYSROOT="$NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/sysroot"
    export CC_aarch64_linux_android="clang --target=aarch64-linux-android${minSdkVersion} --sysroot=$NDK_SYSROOT"
    export CXX_aarch64_linux_android="clang++ --target=aarch64-linux-android${minSdkVersion} --sysroot=$NDK_SYSROOT"
    export CC_armv7_linux_androideabi="clang --target=armv7-linux-androideabi${minSdkVersion} --sysroot=$NDK_SYSROOT"
    export CXX_armv7_linux_androideabi="clang++ --target=armv7-linux-androideabi${minSdkVersion} --sysroot=$NDK_SYSROOT"
    export CC_i686_linux_android="clang --target=i686-linux-android${minSdkVersion} --sysroot=$NDK_SYSROOT"
    export CXX_i686_linux_android="clang++ --target=i686-linux-android${minSdkVersion} --sysroot=$NDK_SYSROOT"
    export CC_x86_64_linux_android="clang --target=x86_64-linux-android${minSdkVersion} --sysroot=$NDK_SYSROOT"
    export CXX_x86_64_linux_android="clang++ --target=x86_64-linux-android${minSdkVersion} --sysroot=$NDK_SYSROOT"

    # Exports the android build tools to path
    export PATH="$ANDROID_HOME/build-tools/${buildToolsVersion}:$ANDROID_HOME/emulator:$ANDROID_HOME/platform-tools:$PATH"
    export LD_LIBRARY_PATH="/run/opengl-driver/lib:/run/opengl-driver-32/lib:${pkgs.lib.makeLibraryPath runtimeLibs}:$LD_LIBRARY_PATH"

    # Override aapt2 with the NixOS-patched version (the SDK's aapt2 is a glibc ELF
    # that can't run without nix-ld).
    export GRADLE_OPTS="''${GRADLE_OPTS:-} -Dandroid.aapt2FromMavenOverride=${aapt2Patched}/bin/aapt2"

    # Create AVDs if they don't exist
    avdmanager list avd | grep -q "Name: android_latest" || yes "" | avdmanager create avd --force -n android_latest -k "system-images;android-${platformVersion};default;x86_64" -p "$ANDROID_AVD_HOME/android_latest.avd"
    avdmanager list avd | grep -q "Name: android_minimum" || yes "" | avdmanager create avd --force -n android_minimum -k "system-images;android-${minSdkVersion};default;x86_64" -p "$ANDROID_AVD_HOME/android_minimum.avd"
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
      cargo-ndk
      gradle
      xdg-utils
      pkg-config
      mold
      clang
      sccache
      upx
      android_latest
      android_minimum
      build_android_apk
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
