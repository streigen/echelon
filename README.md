# Echelon

A cross-platform matrix client focus-built for the gaming community. 

> [!NOTE]
> The main git is on https://git.flaxeneel2.net/streigen/echelon . Github is just a push mirror

> [!NOTE]
> This branch contains the slint rewrite of the application. More details [here](#slint)

## Contributing

The best way to contribute to us is by letting us know about any features you'd like to see in this app, or any bugs that you find. Currently pull requests are closed but we will work on opening that up once we have stable core app. 

## Building

### Pre requisites

- Rust
- Android studio with android SDK 36 and SDK 26. (if you plan on building for android)
- Xcode (if you plan on building for macOS/iOS)

If you are on NixOS, you can use the provided `shell.nix` to get a development environment with all the necessary dependencies.

Run `nix-shell shell.nix` to enter the development environment.

Alternatively, you can use the provided direnv to automatically enable the environment when you cd into the project root.

Ensure this is inside your configuration.nix:

```nix
{
  # Allows the direnv manager to always be available
  programs.direnv = {
    enable = true;
    nix-direnv.enable = true;

    # This ensures the shell integration is loaded for bash/zsh
    enableBashIntegration = true;
    enableZshIntegration = true;
    enableFishIntegration = true;
  };
}
```

Then restart your terminal, and cd into the project directory and enable direnv by running `direnv allow` and the environment should work!


### Development and Building for Desktop

To build the app, run:

```sh
cargo build
```

### Development and Building for Android

Currently, the app supports from android sdk version 26-36 (so, android 8 (oreo) to android 16 (baklava)).

To ensure the functioning of the app, all builds of echelon must be tested on both versions.

#### Building for android

To build for android, we use Cargo NDK.

Run the following command:

```sh
./scripts/android-build.sh [--release|--release-compact] # (Default is debug)
```

If you are on nix, this is also available as `android_build`

#### Running the emulators

The nix shell comes with both emulators ready. To run them, you can run `android_emulator_{latest,minimum}` based on what you need.

#### Installing the app 

> [!NOTE]
> Your APK path might be different based on build type and arch, though you will find your final built APK under
> `android/app/build/outputs/apk` folder.

To install the app in the emulator/on your device, you need to first build the app and then run:

```sh
adb install -r android/app/build/outputs/apk/release/app-x86_64-release.apk
```

#### Debugging

To debug the app, you can use logcat to view the logs.

```sh
adb logcat -s 'RustStdoutStderr:* AndroidRuntime:E DEBUG:* *:F'
```

## Slint

### Why move away from tauri?

Tauri has been amazing to work with, with their mobile support being huge for us to make this app truly cross-platform.

However, during our development process our goals became more ambitious, with the majority of the team in favour of building with a native UI to maximise our resource efficiency.


### Licensed Software

All software used can be found inside [THIRD_PARTY_LICENSES.md](https://github.com/flaxeneel2/echelon/tree/master/static/THIRD_PARTY_LICENSES.md), or if the URL is broken for some reason;

At ```static/THIRD_PARTY_LICENSES.md``` inside this repository.

