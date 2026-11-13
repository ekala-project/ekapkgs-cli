{
  description = "ekapkgs-cli: nix CLI wrapper with negotiated binary cache";

  inputs = {
    utils.url = "github:numtide/flake-utils";
    nixpkgs.url = "github:nixos/nixpkgs/nixpkgs-unstable";
    treefmt-nix.url = "github:numtide/treefmt-nix";
    fenix.url = "github:nix-community/fenix";
    fenix.inputs.nixpkgs.follows = "nixpkgs";
    iets-src.url = "github:numtide/iets";
    iets-src.flake = false;
  };

  outputs =
    {
      self,
      nixpkgs,
      utils,
      treefmt-nix,
      fenix,
      iets-src,
    }:
    let
      localOverlay = nixpkgs.lib.composeExtensions (import ./nix/overlay.nix) (
        final: prev: { iets = prev.iets.override { inherit iets-src; }; }
      );
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
        iets = legacyPackages.iets;
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

      nixosModules.default = self.nixosModules.ekapkgs-serve;
      nixosModules.ekapkgs-serve = {
        imports = [ ./nix/module.nix ];
        nixpkgs.overlays = [ localOverlay ];
      };
    };
}
