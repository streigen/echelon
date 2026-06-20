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
        common = import ./common.nix {
          inherit system nixpkgs rust-overlay;
        };
      in
      {
        devShells.default = common.shell;
      }
    );
}
