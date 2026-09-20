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

        commonArgs = {
          src = pkgs.lib.cleanSourceWith {
            src = ./.;
            filter =
              path: type:
              (pkgs.lib.strings.hasSuffix "src/commit-msg" path) || (craneLib.filterCargoSources path type);
          };
          strictDeps = true;

          buildInputs = with pkgs; [
            openssl
          ];
          nativeBuildInputs = with pkgs; [
            pkg-config
          ];
        };

        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        praddle = craneLib.buildPackage (
          commonArgs
          // {
            inherit cargoArtifacts;
            # FIXME: tests currently require something that is impure
            doCheck = false;
          }
        );
      in
      {
        checks = {
          inherit praddle;
        };

        packages = rec {
          inherit praddle;
          default = praddle;
        };

        devShells.default = craneLib.devShell {
          inherit cargoArtifacts;
          checks = self.checks.${system};
          packages = with pkgs; [
            gh
          ];
        };

        formatter = pkgs.nixfmt-tree;
      }
    );
}
