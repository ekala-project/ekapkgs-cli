final: prev: with final; {
  dev-shell = callPackage ./dev-shell.nix { };

  ekapkgs = callPackage ./package-client.nix { };

  ekapkgs-serve = callPackage ./package-server.nix { };
}
