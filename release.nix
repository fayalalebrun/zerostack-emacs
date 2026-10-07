{
  system ? builtins.currentSystem,
  nixpkgs ? <nixpkgs>,
}:

let
  # TODO: use an ‘input pinner’ to lock in a stable Nixpkgs version
  # <nixpkgs> uses the system’s Nixpkgs version which is impure.
  pkgs = import nixpkgs { 
    inherit system; 
    overlays = [
      (import (builtins.fetchTarball "https://github.com/oxalica/rust-overlay/archive/8afee9fa8caa877a4feb65adc33e8d72f3747dc9.tar.gz"))
      (final: prev: let
        toolchain = final.rust-bin.stable."1.99.0".default.override {
          extensions = [ "clippy" "rustfmt" "rust-src" "rust-analyzer" ];
        };
      in {
        zerostack-toolchain = toolchain;
      })
      (final: prev: {
        zerostack = final.callPackage ./nix/package/zerostack.nix {
          rustPlatform = final.makeRustPlatform {
            cargo = final.zerostack-toolchain;
            rustc = final.zerostack-toolchain;
          };
        };
      })
      (import ./nix/overlay/development.nix)
    ];
  };
in
{
  inherit (pkgs) zerostack;
  default = pkgs.zerostack;
  shell = pkgs.zerostack-dev-shell;
}
