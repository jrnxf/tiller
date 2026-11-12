# Home-manager module: installs tiller and runs `tiller daemon` as a user
# service, a launchd agent on macOS and a systemd user unit on Linux
# (`protocol.md` 6.4).
#
# The service runs the profile's `tiller`, and its environment carries the
# package's store path in TILLER_PACKAGE. A rebuild that changes the package
# therefore changes the launchd plist and the systemd unit, and home-manager's
# activation reloads the agent or restarts the unit, so the daemon runs the
# new binary. A rebuild that keeps the package leaves both untouched and the
# daemon running. `checks.moduleRestartsDaemon` in the flake guards this, and
# the service PATH (Homebrew on macOS only).
#
# The holder (`programs.tiller.holder.enable`, off by default until the
# cutover) is a second, separate service: a sibling of the daemon, never its
# child, because the daemon's restarts must not take the agents' terminals
# with them (`terminal-holder.md` 2.2, 11.2). Its definition holds no store
# path, so a rebuild leaves it as it was and it keeps running; the new binary
# reaches it through `tiller holder upgrade`. `checks.moduleHolderService` in
# the flake guards this.
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
  ]
  ++ lib.concatMap (host: [
    "--allow-host"
    host
  ]) cfg.daemon.allowHosts;
  # herdr, git, gh and quota-axi come from the profile, extraPath, the
  # nix-darwin and NixOS system profiles, Homebrew (macOS) or the system.
  servicePath =
    extraPath:
    lib.concatStringsSep ":" (
      [ profileBin ]
      ++ extraPath
      ++ [
        "/etc/profiles/per-user/${config.home.username}/bin"
        "/run/current-system/sw/bin"
      ]
      ++ lib.optional pkgs.stdenv.hostPlatform.isDarwin "/opt/homebrew/bin"
      ++ [
        "/nix/var/nix/profiles/default/bin"
        "/usr/bin"
        "/bin"
        "/usr/sbin"
        "/sbin"
      ]
    );
  environment = {
    PATH = servicePath cfg.daemon.extraPath;
  }
  // cfg.daemon.environment
  // {
    # Ties the service definition to the package; see the header. Last, so
    # `daemon.environment` cannot drop it.
    TILLER_PACKAGE = "${cfg.package}";
  };
  log = "${config.home.homeDirectory}/Library/Logs/tiller-daemon.log";

  # The holder: the profile's `tiller`, by its stable path. Its environment
  # is the shells' and agents' too, since every pane's child inherits it, and
  # it has no TILLER_PACKAGE: nothing in it names a store path.
  holderCommand = [
    "${profileBin}/tiller"
    "holder"
  ];
  holderEnvironment = {
    PATH = servicePath cfg.holder.extraPath;
  }
  // cfg.holder.environment;
  holderLog = "${config.home.homeDirectory}/Library/Logs/tiller-holder.log";
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
      allowHosts = lib.mkOption {
        # A Host value: a name, an IPv4 or a bracketed IPv6 address, and an
        # optional port. The daemon checks the port range.
        type = lib.types.listOf (
          lib.types.strMatching "([[][0-9A-Fa-f:.]+[]]|[A-Za-z0-9.-]+)(:[0-9]{1,5})?"
        );
        default = [ ];
        example = [ "tiller.example.com" ];
        description = ''
          Host names a reverse proxy serves the board at over https, each
          passed as `--allow-host`. The board answers there only to a
          browser signed in as the owner: the `tiller board --login` code or
          link signs in at each name (the link is also printed for it, with a
          QR code), and mutations are taken only from its `https://` origin.
          Exact names only, no wildcards; the daemon still binds 127.0.0.1
          alone. The proxy should append the browser's address to
          `X-Forwarded-For`, which the daemon reads only to count sign-in
          code tries per browser.
        '';
      };
      extraPath = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [ ];
        example = [ "/home/user/.local/bin" ];
        description = ''
          Directories on the daemon's PATH after the home-manager profile, for
          herdr, git, gh or quota-axi installed elsewhere. The PATH also has
          `/etc/profiles/per-user/<user>/bin` and `/run/current-system/sw/bin`
          after these, and `/opt/homebrew/bin` on macOS, so tools from
          nix-darwin, NixOS and Homebrew resolve without this option.
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
    holder = {
      enable = lib.mkOption {
        type = lib.types.bool;
        default = false;
        description = ''
          Run `tiller holder` as a user service of its own: a launchd agent
          on macOS, a systemd user unit on Linux. The holder keeps the
          agents' terminals. Off until the cutover to the holder
          (`terminal-holder.md` 11.2), so a rebuild before it starts none on
          a live host.

          A rebuild never restarts or stops it, whatever changes in the
          package: its definition names the profile's `tiller` and no store
          path. A new binary reaches the running holder through
          `tiller holder upgrade`.
        '';
      };
      extraPath = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [ ];
        example = [ "/home/user/.local/bin" ];
        description = ''
          Directories on the holder's PATH after the home-manager profile.
          Every pane's shell and agent inherits it.
        '';
      };
      environment = lib.mkOption {
        type = lib.types.attrsOf lib.types.str;
        default = { };
        example = {
          TILLER_LOG = "debug";
        };
        description = ''
          Extra environment for the holder, such as `TILLER_LOG`, or the
          `TILLER_HOME` the daemon runs with. Panes inherit it.
        '';
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

      (lib.mkIf (cfg.holder.enable && pkgs.stdenv.hostPlatform.isDarwin) {
        launchd.agents.tiller-holder = {
          enable = true;
          config = {
            ProgramArguments = holderCommand;
            EnvironmentVariables = holderEnvironment;
            RunAtLoad = true;
            # Starts again after a crash or a kill, and stays down after a
            # clean stop (exit 0), as a holder that already runs exits.
            KeepAlive.SuccessfulExit = false;
            ThrottleInterval = 10;
            # Not "Background": every pane's shell and agent is a child of the
            # holder, and launchd would throttle their CPU and I/O.
            ProcessType = "Standard";
            StandardOutPath = holderLog;
            StandardErrorPath = holderLog;
          };
        };
      })

      (lib.mkIf (cfg.holder.enable && pkgs.stdenv.hostPlatform.isLinux) {
        systemd.user.services.tiller-holder = {
          Unit = {
            Description = "Tiller holder: the agents' terminals";
            # A switch never restarts or stops it; the live upgrade replaces
            # its program (`terminal-holder.md` 6.2). X-SwitchMethod is the
            # switcher's own spelling of the same.
            X-RestartIfChanged = false;
            X-StopIfChanged = false;
            X-SwitchMethod = "keep-old";
            StartLimitIntervalSec = 600;
            StartLimitBurst = 3;
          };
          Service = {
            ExecStart = lib.escapeShellArgs holderCommand;
            Environment = lib.mapAttrsToList (name: value: "\"${name}=${value}\"") holderEnvironment;
            Restart = "on-failure";
            RestartSec = 2;
            # A stop ends the holder alone: its panes' children are not the
            # unit's to kill, and a restart after a crash starts an empty
            # holder (`terminal-holder.md` 6.4).
            KillMode = "process";
          };
          Install.WantedBy = [ "default.target" ];
        };
      })
    ]
  );
}
