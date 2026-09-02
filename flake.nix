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
          # The CLI has to supply the same bindgen schema as the `wasm-bindgen`
          # crate the workspace links, or `just harness` aborts rather than
          # emitting bad bindings. Schemas span several releases -- 0.2.126
          # declares 0.2.122 -- so this pins a known-good release rather than
          # implying release equality. nixpkgs follows its own default, which
          # drifts independently, hence the explicit version.
          #
          # `wasm-bindgen = "0.2"` with an ignored Cargo.lock means the crate
          # can resolve past this on its own. That surfaces as the harness's
          # schema error, which names both versions and the command to fix it.
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
