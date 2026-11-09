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

  storagePath = cfg.settings.storage.path or null;

  credentialPath = "/run/credentials/ekapkgs-serve.service/signing-key";

  generatedKeyDir = "/var/lib/${cfg.stateDirectory}-keys";
  generatedKeyFile = "${generatedKeyDir}/secret.key";
  signingKeyFile = if cfg.signingKeyFile != null then cfg.signingKeyFile else generatedKeyFile;
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
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = ''
        Path to the nix signing secret key file. When null, a key named
        `''${config.networking.hostName}-ekapkgs-1` is generated on first start
        under /var/lib/''${stateDirectory}-keys.
      '';
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

    socketActivation = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Whether to use systemd socket activation instead of direct binding.";
    };

    openFirewall = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Whether to open the firewall for the server port.";
    };

  };

  config = lib.mkIf cfg.enable {
    # Required by the config schema; the --signing-key flag (populated via
    # LoadCredential) is what the server actually reads.
    services.ekapkgs-serve.settings.signing.secret_key_file = lib.mkDefault credentialPath;

    environment.systemPackages = [
      cfg.package
      pkgs.ekapkgs
    ];

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

    systemd.sockets.ekapkgs-serve = lib.mkIf cfg.socketActivation {
      description = "ekapkgs binary cache server socket";
      wantedBy = [ "sockets.target" ];

      socketConfig = {
        ListenStream =
          let
            bind = cfg.settings.server.bind or "127.0.0.1:8080";
          in
          bind;
        ReusePort = true;
      };
    };

    # ReadWritePaths entries must exist before the mount namespace is set up,
    # which happens before systemd would let the service create them.
    systemd.tmpfiles.rules = lib.optional (
      storagePath != null
    ) "d ${storagePath} 0750 ${cfg.user} ${cfg.group} -";

    systemd.services.ekapkgs-serve-keygen = lib.mkIf (cfg.signingKeyFile == null) {
      description = "Generate ekapkgs-serve signing key";
      wantedBy = [ "ekapkgs-serve.service" ];
      before = [ "ekapkgs-serve.service" ];
      unitConfig.ConditionPathExists = "!${generatedKeyFile}";
      serviceConfig.Type = "oneshot";
      script = ''
        install -d -m 0700 ${generatedKeyDir}
        umask 0077
        ${lib.getExe config.nix.package} --extra-experimental-features nix-command \
          key generate-secret --key-name ${config.networking.hostName}-ekapkgs-1 \
          > ${generatedKeyFile}.tmp
        mv ${generatedKeyFile}.tmp ${generatedKeyFile}
      '';
    };

    systemd.services.ekapkgs-serve = {
      description = "ekapkgs binary cache server";
      wantedBy = lib.mkIf (!cfg.socketActivation) [ "multi-user.target" ];
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];

      serviceConfig = {
        Type = "notify";
        WatchdogSec = 15;
        LoadCredential = [ "signing-key:${signingKeyFile}" ];
        ExecStart = lib.escapeShellArgs [
          "${cfg.package}/bin/ekapkgs-serve"
          "--config"
          configFile
          "--signing-key"
          credentialPath
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
        ProtectKernelLogs = true;
        ProtectClock = true;
        ProtectControlGroups = true;
        RestrictSUIDSGID = true;
        RestrictAddressFamilies = [
          "AF_UNIX"
          "AF_INET"
          "AF_INET6"
        ];
        LockPersonality = true;
        MemoryDenyWriteExecute = true;
        SystemCallFilter = [
          "@system-service"
          "~@privileged"
          "~@resources"
        ];
        CapabilityBoundingSet = "";
        LimitNOFILE = 65536;
        ProcSubset = "pid";
        ProtectProc = "invisible";
        SystemCallArchitectures = "native";
        RestrictNamespaces = true;
        RestrictRealtime = true;
        DevicePolicy = "closed";
        UMask = "0077";

        ReadWritePaths = [
          "/nix/var/nix/daemon-socket"
          "/var/lib/${cfg.stateDirectory}"
        ]
        ++ lib.optional (storagePath != null) storagePath;
      };
    };

    networking.firewall.allowedTCPPorts =
      let
        port = cfg.settings.server.bind or "127.0.0.1:8080";
        portNum = lib.toInt (lib.last (lib.splitString ":" port));
      in
      lib.mkIf cfg.openFirewall [ portNum ];
  };

  meta.maintainers = [ ];
}
