let
  rust_overlay_src = builtins.fetchTarball {
    url = "https://github.com/oxalica/rust-overlay/archive/master.tar.gz";
    sha256 = "0qgrkgc695a7gja83dngxrcx4gdg9056gvg5325i5yyjxg0ni6c9";
  };

  common = import ./common.nix {
    nixpkgs = <nixpkgs>;
    rust-overlay = rust_overlay_src;
    system = builtins.currentSystem;
  };
in
common.shell
