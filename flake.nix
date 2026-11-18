{
  description = "Building static binaries with musl";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    crane.url = "github:ipetkov/crane";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      self,
      nixpkgs,
      crane,
      flake-utils,
      ...
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = nixpkgs.legacyPackages.${system};

        craneLib = crane.mkLib pkgs;

        src = pkgs.lib.cleanSourceWith {
          src = ./.;
          filter =
            path: type:
            (pkgs.lib.strings.hasInfix "gen" path)
            || (pkgs.lib.strings.hasSuffix "src/commit-msg" path)
            || (craneLib.filterCargoSources path type);
        };

        commonArgs = {
          inherit src;
          inherit (craneLib.crateNameFromCargoToml { cargoToml = ./crates/praddle/Cargo.toml; })
            pname
            version
            ;
          strictDeps = true;

          buildInputs = with pkgs; [
            openssl
          ];
          nativeBuildInputs = with pkgs; [
            cargo-hakari
            curl
            gh
            git
            pkg-config
          ];
        };

        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        praddle = craneLib.buildPackage (
          commonArgs
          // {
            inherit cargoArtifacts;
            # we use nextest below
            doCheck = false;
          }
        );
      in
      {
        checks = {
          inherit praddle;
          clippy = craneLib.cargoClippy (
            commonArgs
            // {
              inherit cargoArtifacts;
              cargoClippyExtraArgs = "--all-targets -- -D warnings";
            }
          );
          fmt = craneLib.cargoFmt (
            commonArgs
            // {
              inherit src;
            }
          );
          nextest = craneLib.cargoNextest (
            commonArgs
            // {
              inherit cargoArtifacts;
              partitions = 1;
              partitionType = "count";
              cargoNextestPartitionsExtraArgs = "--no-tests=pass";
            }
          );
          hakari = craneLib.mkCargoDerivation (
            commonArgs
            // {
              inherit src;
              cargoArtifacts = null;
              doInstallCargoArtifacts = false;

              buildPhaseCargoCommand = ''
                cargo hakari generate --diff  # workspace-hack Cargo.toml is up-to-date
                cargo hakari manage-deps --dry-run  # all workspace crates depend on workspace-hack
                cargo hakari verify
              '';
            }
          );
        };

        packages = rec {
          inherit praddle;
          default = praddle;
        };

        devShells.default = craneLib.devShell {
          inherit cargoArtifacts;
          checks = self.checks.${system};
          packages = commonArgs.nativeBuildInputs;
        };

        formatter = pkgs.nixfmt-tree;
      }
    );
}
