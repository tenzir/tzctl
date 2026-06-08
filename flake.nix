{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/master";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
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
          overlays = [
            rust-overlay.overlays.default
          ];
        };

        package = pkgs.rustPlatform.buildRustPackage {
          pname = "tzctl";
          version = "0.1.0";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
          meta = {
            description = "Manage Tenzir pipelines through the Tenzir Platform";
            homepage = "https://github.com/tenzir/tzctl";
            license = pkgs.lib.licenses.asl20;
            mainProgram = "tzctl";
          };
        };

        # dev shell
        devShell = pkgs.mkShell {
          buildInputs = [
            pkgs.rust-bin.stable.latest.default
            pkgs.rust-analyzer
            pkgs.just

            # Python
            pkgs.maturin
            pkgs.python3
            pkgs.uv
          ];
        };
      in
      {
        packages.default = package;
        devShells.default = devShell;
      }
    );
}
