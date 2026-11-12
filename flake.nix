{
  description = "ekapkgs-cli: nix CLI wrapper with negotiated binary cache";

  inputs = {
    utils.url = "github:numtide/flake-utils";
    nixpkgs.url = "github:nixos/nixpkgs/nixpkgs-unstable";
    treefmt-nix.url = "github:numtide/treefmt-nix";
    fenix.url = "github:nix-community/fenix";
    fenix.inputs.nixpkgs.follows = "nixpkgs";
    # ekaos, for the VM tests of `ekapkgs system`
    corepkgs.url = "github:ekala-project/corepkgs";
    corepkgs.inputs.treefmt-nix.follows = "treefmt-nix";
  };

  outputs =
    {
      self,
      nixpkgs,
      utils,
      treefmt-nix,
      fenix,
      corepkgs,
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

      # VM tests: `nix build .#checks.x86_64-linux.ekaos-switch`
      checks = nixpkgs.lib.optionalAttrs (system == "x86_64-linux") {
        ekaos-switch = import ./nix/tests/ekaos-switch.nix {
          inherit (corepkgs.legacyPackages.${system}) ekaosTest;
          inherit (legacyPackages) ekapkgs;
        };
      };
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
