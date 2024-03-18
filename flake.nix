{
  description = "A stack-based array programming language";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  inputs.flake-utils.url = "github:numtide/flake-utils";
  inputs.crane = {
    url = "github:ipetkov/crane";
    inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs = { self, nixpkgs, flake-utils, crane }:
    flake-utils.lib.eachDefaultSystem (system:
    let
      pkgs = nixpkgs.legacyPackages.${system};
      craneLib = crane.lib.${system};
      uiua-crate = craneLib.buildPackage {
        cargoExtraArgs = "--locked --features  libffi/system";
        src = craneLib.cleanCargoSource (craneLib.path ./.);
        buildInputs = (nixpkgs.lib.optionals pkgs.stdenv.isDarwin [
            pkgs.iconv
            pkgs.darwin.apple_sdk.frameworks.CoreServices
            pkgs.darwin.apple_sdk.frameworks.Foundation
          ]) ++ [
            pkgs.libffi
          ];
      };
    in
      {
        packages.default = uiua-crate;
        devShell = craneLib.devShell {
          inputsFrom = builtins.attrValues self.packages.${system};
          nativeBuildInputs = [
            pkgs.clippy
            pkgs.rust-analyzer
            pkgs.rustfmt
          ];
        };
      });
}
