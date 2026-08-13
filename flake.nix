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
          targets = [ "wasm32-unknown-unknown" ];
        };

        tools = [
          rust-toolchain
          pkgs.cargo-shear
          pkgs.cargo-sort
          pkgs.cargo-edit
          pkgs.cargo-hack
          pkgs.just
          pkgs.bun
          # The bindgen schema is unstable, so the CLI has to match the
          # `wasm-bindgen` crate the workspace links exactly -- `just harness`
          # aborts on a mismatch rather than emitting bad bindings. nixpkgs
          # tracks its own default, so pin the version instead of following it.
          pkgs.wasm-bindgen-cli_0_2_126
          pkgs.python312
          pkgs.uv
          pkgs.pkg-config
          pkgs.glib
          pkgs.gtk3
          pkgs.cmake
          # Required to compile boringssl (via bindgen loading libclang)
          pkgs.llvmPackages.libclang.lib
          # Only for NPM publishing
          pkgs.nodejs_24
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
