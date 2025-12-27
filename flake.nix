{
  description = "Web Transport development environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      rust-overlay,
      ...
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };

        rust-toolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [
            "rust-src"
            "rust-analyzer"
          ];
        };

        tools = [
          rust-toolchain
          pkgs.cargo-shear
          pkgs.cargo-sort
          pkgs.cargo-edit
          pkgs.cargo-hack
          pkgs.just
          pkgs.bun
          pkgs.pkg-config
          pkgs.glib
          pkgs.gtk3
          # Required to compile boringssl (via bindgen loading libclang)
          pkgs.llvmPackages.libclang.lib
        ];
      in
      {
        devShells.default = pkgs.mkShell {
          packages = tools;

          shellHook = ''
            export LD_LIBRARY_PATH=${
              pkgs.lib.makeLibraryPath [ pkgs.llvmPackages.libclang.lib ]
            }:$LD_LIBRARY_PATH
          '';
        };
      }
    );
}
