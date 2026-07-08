let
  rust_overlay_src = builtins.fetchTarball {
    url = "https://github.com/oxalica/rust-overlay/archive/master.tar.gz";
    sha256 = "1y94bw0zi92c3qr584ycy89pb7385wxbv9prxwrzs2z2gpw9yrcf";
  };

  common = import ./common.nix {
    nixpkgs = <nixpkgs>;
    rust-overlay = rust_overlay_src;
    system = builtins.currentSystem;
  };
in
common.shell
