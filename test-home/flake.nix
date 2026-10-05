{
  description = "Test flake for ekapkgs home e2e";

  inputs.corepkgs.url = "github:ekala-project/corepkgs/jonringer/user-env";

  outputs =
    { corepkgs, ... }:
    let
      system = "x86_64-linux";
      pkgs = corepkgs.legacyPackages.${system};
      evalHome = import "${corepkgs}/ekaos/eval-home.nix" { inherit (pkgs) lib; inherit pkgs; };

      result = evalHome {
        modules = [
          {
            home.users.jon = {
              packages = [
                pkgs.diffutils
              ];

              sessionVariables = {
                EDITOR = "vi";
              };

              sessionPath = [
                "$HOME/.local/bin"
              ];

              programs.bash.enable = true;
            };
          }
        ];
      };
    in
    {
      # Standalone eval path — what the CLI auto-detects first
      config.home.build.activationPackage = result.activationPackage;
      config.home.build.activationPackages = result.activationPackages;

      # Full system compat alias
      config.system.build.home = result.activationPackage;
    };
}
