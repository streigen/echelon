# Echelon

A (hopefully) good cross-platform client for matrix servers.

> [!NOTE]
> The main git is on https://git.flaxeneel2.net/streigen/echelon . Github is just a push mirror

## Contributing

Currently, this is just a personal project with some uni friends, but we are always open for feature requests and bug reports.

## Building

### Pre requisites

- Rust
- Bun (Node.js probably works too, but we use bun for development)
- Android studio with android SDK 35. (if you plan on building for android)
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
  };
}
```

Then restart your terminal, and cd into the project directory and enable direnv by using the command it displays and the environment should work!

### Installing dependencies

To install dependencies, run:

```sh
bun install
```

### Development and Building for Desktop

To build for desktop, run:

```sh
bun run tauri build
```

### Development and Building for Android

#### Linux Devices 
To run the app via an Android emulator, first run the following to start the emulator:

```bash
run-test-emulator
```

Then, to run the application in the emulator:

```bash
bun run tauri android dev
```

We recommend using an Android device if 
your hardware acceleration is broken for the Android SDK on Linux,

To do so, you need to follow the following steps on your device:
1. Go to Settings > About Phone.

2. Tap Build Number 7 times until it says "You are now a developer."

3. Go to Settings > System > Developer Options.

4. Enable USB Debugging.

on NixOS, you need to add this to your config:

```nix
{
    programs.adb.enable = true;
    users.users.YOURUSERNAMEHERE.extraGroups = [ "adbusers" ];
}
```

On non-NixOS systems, you need to ensure your user is in the adbusers group, and you have the
correct udev rules implemented.

On Windows, you should not have any issues (hopefully), but it has not been tested yet.

To run in a dev environment, run the command:
```bash
bun run tauri android dev --host 127.0.0.1
```

To build for android, run:

```sh
bun run tauri android build
```

### Licensed Software

All software used can be found inside [THIRD_PARTY_LICENSES.md](https://github.com/flaxeneel2/echelon/tree/master/static/THIRD_PARTY_LICENSES.md), or if the URL is broken for some reason;

At ```static/THIRD_PARTY_LICENSES.md``` inside this repository.
