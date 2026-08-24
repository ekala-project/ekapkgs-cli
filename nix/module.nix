{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.services.ekapkgs-serve;
  settingsFormat = pkgs.formats.toml { };

  configFile = settingsFormat.generate "ekapkgs-serve.toml" cfg.settings;
in
{
  options.services.ekapkgs-serve = {
    enable = lib.mkEnableOption "the ekapkgs binary cache server";

    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.ekapkgs-serve;
      defaultText = lib.literalExpression "pkgs.ekapkgs-serve";
      description = "The ekapkgs-serve package to run.";
    };

    user = lib.mkOption {
      type = lib.types.str;
      default = "ekapkgs";
      description = "System user the server runs as.";
    };

    group = lib.mkOption {
      type = lib.types.str;
      default = "ekapkgs";
      description = "Group the server runs as.";
    };

    stateDirectory = lib.mkOption {
      type = lib.types.str;
      default = "ekapkgs-serve";
      description = "Subdirectory under /var/lib for cache storage.";
    };

    signingKeyFile = lib.mkOption {
      type = lib.types.path;
      description = "Path to the nix signing secret key file.";
    };

    environmentFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = ''
        Path to a file containing additional environment variables.
        Compatible with sops-nix and agenix.
      '';
    };

    settings = lib.mkOption {
      type = settingsFormat.type;
      default = { };
      description = ''
        Configuration for ekapkgs-serve, serialised to TOML and
        passed via --config.
      '';
    };

    openFirewall = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Whether to open the firewall for the server port.";
    };
  };

  config = lib.mkIf cfg.enable {
    users.users = lib.mkIf (cfg.user == "ekapkgs") {
      ekapkgs = {
        isSystemUser = true;
        group = cfg.group;
        home = "/var/lib/${cfg.stateDirectory}";
        createHome = false;
        description = "ekapkgs binary cache server";
      };
    };

    users.groups = lib.mkIf (cfg.group == "ekapkgs") {
      ekapkgs = { };
    };

    systemd.services.ekapkgs-serve = {
      description = "ekapkgs binary cache server";
      wantedBy = [ "multi-user.target" ];
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];

      serviceConfig = {
        Type = "simple";
        ExecStart = lib.escapeShellArgs [
          "${cfg.package}/bin/ekapkgs-serve"
          "--config"
          configFile
          "--signing-key"
          cfg.signingKeyFile
        ];
        Restart = "on-failure";
        RestartSec = "10s";

        User = cfg.user;
        Group = cfg.group;

        StateDirectory = cfg.stateDirectory;
        StateDirectoryMode = "0750";
        RuntimeDirectory = "ekapkgs-serve";
        RuntimeDirectoryMode = "0700";
        WorkingDirectory = "/var/lib/${cfg.stateDirectory}";

        EnvironmentFile = lib.optional (cfg.environmentFile != null) cfg.environmentFile;

        NoNewPrivileges = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateTmp = true;
        PrivateDevices = true;
        ProtectKernelTunables = true;
        ProtectKernelModules = true;
        ProtectControlGroups = true;
        RestrictSUIDSGID = true;
        LockPersonality = true;

        ReadWritePaths = [
          "/nix/var/nix/daemon-socket"
          "/var/lib/${cfg.stateDirectory}"
        ];
      };
    };

    networking.firewall.allowedTCPPorts =
      let
        port = cfg.settings.server.bind or "0.0.0.0:8080";
        portNum = lib.toInt (lib.last (lib.splitString ":" port));
      in
      lib.mkIf cfg.openFirewall [ portNum ];
  };

  meta.maintainers = [ ];
}
