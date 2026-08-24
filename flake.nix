{
  description = "ekapkgs-cli: nix CLI wrapper with negotiated binary cache";

  inputs = {
    utils.url = "github:numtide/flake-utils";
    nixpkgs.url = "github:nixos/nixpkgs/nixpkgs-unstable";
    treefmt-nix.url = "github:numtide/treefmt-nix";
    fenix.url = "github:nix-community/fenix";
    fenix.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs =
    {
      self,
      nixpkgs,
      utils,
      treefmt-nix,
      fenix,
    }:
    let
      localOverlay = import ./nix/overlay.nix;
    in
    utils.lib.eachDefaultSystem (system: rec {
      legacyPackages = import nixpkgs {
        inherit system;
        overlays = [
          localOverlay
          fenix.overlays.default
        ];
      };

      packages = {
        default = legacyPackages.ekapkgs;
        ekapkgs = legacyPackages.ekapkgs;
        ekapkgs-serve = legacyPackages.ekapkgs-serve;
      };
      devShells.default = legacyPackages.dev-shell;
      formatter =
        let
          fmt = treefmt-nix.lib.evalModule legacyPackages {
            programs.rustfmt.enable = true;
            programs.nixfmt.enable = true;
          };
        in
        fmt.config.build.wrapper;
    })
    // {
      overlays.default = localOverlay;

      nixosModules.default = import ./nix/module.nix;
      nixosModules.ekapkgs-serve = import ./nix/module.nix;
    };
}
