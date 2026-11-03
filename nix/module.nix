# Home-manager module: installs tiller and runs `tiller daemon` as a user
# service, a launchd agent on macOS and a systemd user unit on Linux
# (`protocol.md` 6.4).
#
# The service runs the profile's `tiller`, and its environment carries the
# package's store path in TILLER_PACKAGE. A rebuild that changes the package
# therefore changes the launchd plist and the systemd unit, and home-manager's
# activation reloads the agent or restarts the unit, so the daemon runs the
# new binary. A rebuild that keeps the package leaves both untouched and the
# daemon running. `checks.moduleRestartsDaemon` in the flake guards this.
#
# Both service managers restart the daemon when it dies (a crash, a kill, or
# exit 3 for a database newer than the binary, `recovery.md` R11) and leave it
# down after a clean exit 0: a stop, or a second instance that found the first
# running.
self:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.programs.tiller;
  profileBin = "${config.home.profileDirectory}/bin";
  command = [
    "${profileBin}/tiller"
    "daemon"
    "--port"
    (toString cfg.daemon.port)
  ];
  environment = {
    # herdr, git, gh and quota-axi come from the profile or the system.
    PATH = lib.concatStringsSep ":" (
      [ profileBin ]
      ++ cfg.daemon.extraPath
      ++ [
        "/nix/var/nix/profiles/default/bin"
        "/usr/bin"
        "/bin"
        "/usr/sbin"
        "/sbin"
      ]
    );
  }
  // cfg.daemon.environment
  // {
    # Ties the service definition to the package; see the header. Last, so
    # `daemon.environment` cannot drop it.
    TILLER_PACKAGE = "${cfg.package}";
  };
  log = "${config.home.homeDirectory}/Library/Logs/tiller-daemon.log";
in
{
  options.programs.tiller = {
    enable = lib.mkEnableOption "tiller";
    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.tiller;
      defaultText = lib.literalExpression "tiller.packages.\${system}.tiller";
      description = "The tiller package to install.";
    };
    daemon = {
      enable = lib.mkOption {
        type = lib.types.bool;
        default = true;
        description = ''
          Run `tiller daemon` as a user service: a launchd agent that starts
          at login on macOS, a systemd user unit on Linux. On Linux the unit
          starts at boot only when lingering is on for the user
          (`loginctl enable-linger`); otherwise it starts at login.
        '';
      };
      port = lib.mkOption {
        type = lib.types.port;
        default = 7421;
        description = "The board's port. The daemon binds 127.0.0.1 only.";
      };
      extraPath = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [ ];
        example = [ "/opt/homebrew/bin" ];
        description = ''
          Directories on the daemon's PATH after the home-manager profile, for
          herdr, git, gh or quota-axi installed elsewhere.
        '';
      };
      environment = lib.mkOption {
        type = lib.types.attrsOf lib.types.str;
        default = { };
        example = {
          TILLER_LOG = "debug";
        };
        description = "Extra environment for the daemon, such as `TILLER_LOG` or `TILLER_HOME`.";
      };
    };
  };

  config = lib.mkIf cfg.enable (
    lib.mkMerge [
      { home.packages = [ cfg.package ]; }

      (lib.mkIf (cfg.daemon.enable && pkgs.stdenv.hostPlatform.isDarwin) {
        launchd.agents.tiller = {
          enable = true;
          config = {
            ProgramArguments = command;
            EnvironmentVariables = environment;
            RunAtLoad = true;
            KeepAlive.SuccessfulExit = false;
            # launchd has no start limit: it waits this long between starts.
            ThrottleInterval = 10;
            ProcessType = "Background";
            StandardOutPath = log;
            StandardErrorPath = log;
          };
        };
      })

      (lib.mkIf (cfg.daemon.enable && pkgs.stdenv.hostPlatform.isLinux) {
        systemd.user.services.tiller = {
          Unit = {
            Description = "Tiller daemon: the board and the fleet's background work";
            # More than three restarts in ten minutes leaves the board down
            # (`recovery.md` R01).
            StartLimitIntervalSec = 600;
            StartLimitBurst = 3;
          };
          Service = {
            ExecStart = lib.escapeShellArgs command;
            Environment = lib.mapAttrsToList (name: value: "\"${name}=${value}\"") environment;
            Restart = "on-failure";
            RestartSec = 2;
          };
          Install.WantedBy = [ "default.target" ];
        };
      })
    ]
  );
}
