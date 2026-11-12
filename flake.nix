{
  description = "Tiller: a durable board and orchestration layer for coding-agent fleets";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    bun2nix = {
      url = "github:nix-community/bun2nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  nixConfig = {
    extra-substituters = [ "https://nix-community.cachix.org" ];
    extra-trusted-public-keys = [
      "nix-community.cachix.org-1:mB9FSh9qf2dCimDSUo8Zy7bkq5CX+/rkCWyvRCYg3Fs="
    ];
  };

  outputs =
    {
      self,
      nixpkgs,
      crane,
      rust-overlay,
      bun2nix,
      ...
    }:
    let
      # The locked nixpkgs (26.11) has dropped x86_64-darwin, so it is not
      # listed: its dev shell and package cannot evaluate.
      systems = [
        "aarch64-darwin"
        "aarch64-linux"
        "x86_64-linux"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f system);

      perSystem =
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [
              rust-overlay.overlays.default
              bun2nix.overlays.default
            ];
          };
          inherit (pkgs) lib;

          toolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
          craneLib = (crane.mkLib pkgs).overrideToolchain toolchain;

          # bun 1.4 writes lockfileVersion 2, which bun2nix 2.1.2 rejects.
          # The patch is upstream PR 110 and only affects the dev-time converter.
          bun2nixCli = pkgs.bun2nix.overrideAttrs (old: {
            patches = (old.patches or [ ]) ++ [ ./nix/patches/bun2nix-lockfile-v2.patch ];
            # The build's source root is programs/bun2nix, so strip that prefix too.
            patchFlags = [ "-p3" ];
          });

          # The board: bun installs from the bun2nix dependency cache, then vite builds.
          boardSrc = lib.fileset.toSource {
            root = ./board;
            fileset = lib.fileset.difference ./board (
              lib.fileset.unions [
                (lib.fileset.maybeMissing ./board/node_modules)
                (lib.fileset.maybeMissing ./board/dist)
              ]
            );
          };
          board = pkgs.stdenv.mkDerivation {
            pname = "tiller-board";
            version = baseVersion;
            src = boardSrc;
            # vite runs on real node, not bun's node shim.
            nativeBuildInputs = [
              pkgs.bun2nix.hook
              pkgs.nodejs
            ];
            bunDeps = pkgs.bun2nix.fetchBunDeps {
              bunNix = ./board/bun.nix;
              useFakeNode = false;
            };
            dontRunLifecycleScripts = true;
            buildPhase = ''
              runHook preBuild
              bun run build
              runHook postBuild
            '';
            installPhase = ''
              runHook preInstall
              cp -R dist $out
              runHook postInstall
            '';
          };

          # The tiller binary, with the board's static files embedded.
          src = lib.fileset.toSource {
            root = ./.;
            fileset = lib.fileset.unions [
              (craneLib.fileset.commonCargoSources ./.)
              # The core's migrations and test fixtures are SQL that include_str! embeds.
              (lib.fileset.fileFilter (file: file.hasExt "sql") ./crates)
              # insta snapshots, and the nextest overrides the checks run with.
              (lib.fileset.fileFilter (file: file.hasExt "snap") ./crates)
              ./.config/nextest.toml
              ./crates/daemon/tests/fixtures
              # The committed OpenAPI contract a daemon test compares against.
              ./contract
              # Embedded by crates/agents with include_str!.
              ./roles
              # Embedded by crates/replay with include_str!.
              ./crates/replay/prompts
              # The role matrix core::authority::matrix_matches_spec reads with include_str!.
              ./docs/spec/authority.md
              # The board's key names, which a tiller-panes key test parses, and
              # the Claude Code viewport reads the composer, the prompt parser
              # and the holder's model gate are tested on.
              ./crates/panes/tests/fixtures
            ];
          };
          # The version the binary reports. Cargo.toml keeps the base version; the
          # build stamps this one into Cargo.toml and Cargo.lock. A flake cannot see
          # the tag it was fetched at (self has the rev, never the ref), so the
          # commit a v<base>-pre.N tag points at carries the stamp in PRERELEASE
          # (docs/releasing.md). An unstamped build reports <base>-dev+<rev>.
          baseVersion = (fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;
          stamp = lib.removeSuffix "\n" (builtins.readFile ./PRERELEASE);
          version =
            if stamp != "" then
              stamp
            else
              "${baseVersion}-dev+${self.shortRev or self.dirtyShortRev or "unknown"}";
          commonArgs = {
            inherit src;
            pname = "tiller";
            version = baseVersion;
            strictDeps = true;
          };
          cargoArtifacts = craneLib.buildDepsOnly commonArgs;
          tiller = craneLib.buildPackage (
            commonArgs
            // {
              inherit cargoArtifacts version;
              cargoExtraArgs = "--package tiller-cli";
              # The dependency build keeps the base version, so a new tag reuses it.
              postPatch = ''
                sed -i 's/^version = ".*"/version = "${version}"/' Cargo.toml
                sed -i '/^name = "tiller-/{n;s/^version = ".*"/version = "${version}"/}' Cargo.lock
              '';
              doCheck = false;
              preBuild = ''
                mkdir -p board
                cp -R --no-preserve=mode ${board} board/dist
              '';
              meta.mainProgram = "tiller";
            }
          );
        in
        {
          packages = {
            inherit tiller board;
            default = tiller;
          };

          checks = {
            inherit tiller;
            clippy = craneLib.cargoClippy (
              commonArgs
              // {
                inherit cargoArtifacts;
                cargoClippyExtraArgs = "--workspace --all-targets -- -D warnings";
              }
            );
            fmt = craneLib.cargoFmt { inherit src; };
            nextest = craneLib.cargoNextest (
              commonArgs
              // {
                inherit cargoArtifacts;
                # The landing tests drive real git repositories, and the
                # checkout tests read process states with ps.
                nativeCheckInputs = [
                  pkgs.git
                  pkgs.procps
                ];
              }
            );
            bunNixFresh =
              pkgs.runCommand "tiller-bun-nix-fresh"
                {
                  nativeBuildInputs = [ bun2nixCli ];
                }
                ''
                  bun2nix -l ${./board/bun.lock} -o generated.nix
                  diff -u ${./board/bun.nix} generated.nix \
                    || { echo "board/bun.nix is stale: run 'bun run nix:sync' in board/" >&2; exit 1; }
                  touch $out
                '';
            # vendor/alacritty_terminal is the released crate plus the handover
            # patch, and nothing else (vendor/README.md).
            alacrittyPatchFresh =
              pkgs.runCommand "tiller-alacritty-patch-fresh"
                {
                  crate = pkgs.fetchurl {
                    name = "alacritty_terminal-0.26.0.tar.gz";
                    url = "https://static.crates.io/crates/alacritty_terminal/alacritty_terminal-0.26.0.crate";
                    # The checksum Cargo.lock had for the registry crate.
                    sha256 = "bda177466b9524d59f1b12f0dd30b68696788e9992a7e959021c4a0ed96fcf59";
                  };
                  nativeBuildInputs = [ pkgs.patch ];
                }
                ''
                  tar -xzf $crate
                  mkdir upstream
                  cp -R alacritty_terminal-0.26.0/{README.md,LICENSE-APACHE,src} upstream/
                  # The manifest drops the authors (the scrub check rejects any
                  # email address) and the reference tests, which are not vendored.
                  sed '/^authors = \[/,/^\]/d; /^\[\[test\]\]/,/^$/d' \
                    alacritty_terminal-0.26.0/Cargo.toml > upstream/Cargo.toml
                  patch -d upstream -p1 < ${./nix/patches/alacritty_terminal-0.26.0-handover.patch}
                  diff -ru upstream ${./vendor/alacritty_terminal} \
                    || { echo "vendor/alacritty_terminal is not the crate plus the patch (vendor/README.md)" >&2; exit 1; }
                  touch $out
                '';
            # A rebuild restarts the daemon only because the launchd agent and
            # the systemd unit change with the package (see nix/module.nix).
            # Evaluates the module against stub home-manager options with two
            # packages and fails if either service definition stays the same.
            moduleRestartsDaemon =
              let
                stubPackage = name: pkgs.writeText name name;
                serviceFor =
                  isDarwin: package: daemon:
                  let
                    config =
                      (lib.evalModules {
                        modules = [
                          (import ./nix/module.nix self)
                          {
                            options = {
                              home.packages = lib.mkOption { type = lib.types.listOf lib.types.package; };
                              home.profileDirectory = lib.mkOption { type = lib.types.str; };
                              home.homeDirectory = lib.mkOption { type = lib.types.str; };
                              home.username = lib.mkOption { type = lib.types.str; };
                              launchd.agents = lib.mkOption {
                                type = lib.types.attrsOf lib.types.anything;
                                default = { };
                              };
                              systemd.user.services = lib.mkOption {
                                type = lib.types.attrsOf lib.types.anything;
                                default = { };
                              };
                            };
                            config = {
                              home.profileDirectory = "/home/user/.nix-profile";
                              home.homeDirectory = "/home/user";
                              home.username = "user";
                              programs.tiller = {
                                enable = true;
                                inherit package daemon;
                              };
                            };
                          }
                        ];
                        specialArgs.pkgs.stdenv.hostPlatform = {
                          inherit system isDarwin;
                          isLinux = !isDarwin;
                        };
                      }).config;
                  in
                  if isDarwin then config.launchd.agents.tiller else config.systemd.user.services.tiller;
                pathFor =
                  isDarwin:
                  let
                    service = serviceFor isDarwin (stubPackage "tiller-a") { };
                  in
                  if isDarwin then
                    service.config.EnvironmentVariables.PATH
                  else
                    lib.removePrefix "PATH=" (
                      lib.findFirst (lib.hasPrefix "PATH=") "" (
                        map (lib.removeSuffix "\"") (map (lib.removePrefix "\"") service.Service.Environment)
                      )
                    );
                hasDir = isDarwin: dir: builtins.elem dir (lib.splitString ":" (pathFor isDarwin));
                # The arguments after `--port <n>`, for both service managers.
                allowHostArgs =
                  isDarwin:
                  let
                    service = serviceFor isDarwin (stubPackage "tiller-a") {
                      allowHosts = [
                        "tiller.example.com"
                        "board.example.com"
                      ];
                    };
                  in
                  if isDarwin then
                    lib.drop 4 service.config.ProgramArguments
                  else
                    map (word: lib.removePrefix "'" (lib.removeSuffix "'" word)) (
                      lib.drop 4 (lib.splitString " " service.Service.ExecStart)
                    );
                follows =
                  isDarwin:
                  builtins.toJSON (serviceFor isDarwin (stubPackage "tiller-a") { })
                  != builtins.toJSON (serviceFor isDarwin (stubPackage "tiller-b") { });
              in
              assert lib.assertMsg (follows true)
                "nix/module.nix: the launchd agent does not change with the package, so a rebuild leaves the old daemon running";
              assert lib.assertMsg (follows false)
                "nix/module.nix: the systemd unit does not change with the package, so a rebuild leaves the old daemon running";
              assert lib.assertMsg (hasDir true "/opt/homebrew/bin")
                "nix/module.nix: the launchd agent's PATH lacks /opt/homebrew/bin, so Homebrew tools such as herdr are not found";
              assert lib.assertMsg (
                !hasDir false "/opt/homebrew/bin"
              ) "nix/module.nix: the systemd unit's PATH has /opt/homebrew/bin, which only macOS has";
              assert lib.assertMsg (
                hasDir true "/run/current-system/sw/bin" && hasDir false "/run/current-system/sw/bin"
              ) "nix/module.nix: a service PATH lacks /run/current-system/sw/bin";
              assert lib.assertMsg (
                hasDir true "/etc/profiles/per-user/user/bin" && hasDir false "/etc/profiles/per-user/user/bin"
              ) "nix/module.nix: a service PATH lacks /etc/profiles/per-user/<user>/bin";
              assert lib.assertMsg (
                allowHostArgs true == [
                  "--allow-host"
                  "tiller.example.com"
                  "--allow-host"
                  "board.example.com"
                ]
                && allowHostArgs false == allowHostArgs true
              ) "nix/module.nix: daemon.allowHosts does not reach the daemon as --allow-host arguments";
              pkgs.runCommand "tiller-module-restarts-daemon" { } "touch $out";
            # The holder service (`terminal-holder.md` 2.2, 11.2) is off by
            # default; on, its launchd agent and systemd unit name the
            # profile's `tiller` and no store path, so they stay the same
            # across packages and a switch never restarts the holder.
            moduleHolderService =
              let
                stubPackage = name: pkgs.writeText name name;
                configFor =
                  isDarwin: tiller:
                  (lib.evalModules {
                    modules = [
                      (import ./nix/module.nix self)
                      {
                        options = {
                          home.packages = lib.mkOption { type = lib.types.listOf lib.types.package; };
                          home.profileDirectory = lib.mkOption { type = lib.types.str; };
                          home.homeDirectory = lib.mkOption { type = lib.types.str; };
                          home.username = lib.mkOption { type = lib.types.str; };
                          launchd.agents = lib.mkOption {
                            type = lib.types.attrsOf lib.types.anything;
                            default = { };
                          };
                          systemd.user.services = lib.mkOption {
                            type = lib.types.attrsOf lib.types.anything;
                            default = { };
                          };
                        };
                        config = {
                          home.profileDirectory = "/home/user/.nix-profile";
                          home.homeDirectory = "/home/user";
                          home.username = "user";
                          programs.tiller = tiller;
                        };
                      }
                    ];
                    specialArgs.pkgs.stdenv.hostPlatform = {
                      inherit system isDarwin;
                      isLinux = !isDarwin;
                    };
                  }).config;
                on = package: {
                  enable = true;
                  inherit package;
                  holder = {
                    enable = true;
                    extraPath = [ "/home/user/.local/bin" ];
                    environment.TILLER_HOME = "/home/user/.tiller";
                  };
                };
                agent = isDarwin: package: (configFor isDarwin (on package)).launchd.agents.tiller-holder;
                unit = isDarwin: package: (configFor isDarwin (on package)).systemd.user.services.tiller-holder;
                a = stubPackage "tiller-a";
                b = stubPackage "tiller-b";
                off =
                  isDarwin:
                  configFor isDarwin {
                    enable = true;
                    package = a;
                  };
                # The Linux environment as an attrset.
                envOf =
                  service:
                  lib.listToAttrs (
                    map (
                      word:
                      let
                        parts = lib.splitString "=" (lib.removeSuffix "\"" (lib.removePrefix "\"" word));
                      in
                      lib.nameValuePair (lib.head parts) (lib.concatStringsSep "=" (lib.tail parts))
                    ) service.Service.Environment
                  );
                path =
                  isDarwin:
                  lib.splitString ":" (
                    if isDarwin then (agent true a).config.EnvironmentVariables.PATH else (envOf (unit false a)).PATH
                  );
                mentionsStore = value: lib.hasInfix builtins.storeDir (builtins.toJSON value);
              in
              assert lib.assertMsg (
                (off true).launchd.agents == { } || !(off true).launchd.agents ? tiller-holder
              ) "nix/module.nix: the launchd holder agent exists with holder.enable off";
              assert lib.assertMsg (
                !(off false).systemd.user.services ? tiller-holder
              ) "nix/module.nix: the systemd holder unit exists with holder.enable off";
              assert lib.assertMsg (
                (agent true a).config.ProgramArguments == [
                  "/home/user/.nix-profile/bin/tiller"
                  "holder"
                ]
              ) "nix/module.nix: the launchd holder agent does not run the profile's tiller holder";
              assert lib.assertMsg (
                (unit false a).Service.ExecStart == "/home/user/.nix-profile/bin/tiller holder"
              ) "nix/module.nix: the systemd holder unit does not run the profile's tiller holder";
              assert lib.assertMsg (builtins.toJSON (agent true a) == builtins.toJSON (agent true b))
                "nix/module.nix: the launchd holder agent changes with the package, so a switch would reload the holder";
              assert lib.assertMsg (builtins.toJSON (unit false a) == builtins.toJSON (unit false b))
                "nix/module.nix: the systemd holder unit changes with the package, so a switch would restart the holder";
              assert lib.assertMsg (
                !mentionsStore (agent true a) && !mentionsStore (unit false a)
              ) "nix/module.nix: a holder service definition names a store path";
              assert lib.assertMsg (
                (unit false a).Unit.X-RestartIfChanged == false
                && (unit false a).Unit.X-StopIfChanged == false
                && (unit false a).Unit.X-SwitchMethod == "keep-old"
              ) "nix/module.nix: the systemd holder unit can be restarted or stopped by a switch";
              assert lib.assertMsg (
                (unit false a).Service.KillMode == "process" && (unit false a).Service.Restart == "on-failure"
              ) "nix/module.nix: the systemd holder unit lacks KillMode=process or Restart=on-failure";
              assert lib.assertMsg (
                (agent true a).config.KeepAlive.SuccessfulExit == false && (agent true a).config.RunAtLoad
              ) "nix/module.nix: the launchd holder agent does not start at load and restart after a crash";
              assert lib.assertMsg (
                (agent true a).config.ProcessType == "Standard"
              ) "nix/module.nix: the launchd holder agent is throttled: its panes' processes inherit ProcessType";
              # The plist home-manager writes for the agent.
              assert lib.assertMsg
                (
                  let
                    plist = lib.generators.toPlist { } (agent true a).config;
                  in
                  lib.hasInfix "<key>ProgramArguments</key>" plist
                  && lib.hasInfix "<string>/home/user/.nix-profile/bin/tiller</string>" plist
                  && lib.hasInfix "<string>holder</string>" plist
                  && lib.hasInfix "<key>SuccessfulExit</key>" plist
                  && lib.hasInfix "<key>RunAtLoad</key>" plist
                  && lib.hasInfix "/home/user/Library/Logs/tiller-holder.log" plist
                  && !lib.hasInfix builtins.storeDir plist
                )
                "nix/module.nix: the launchd holder agent's plist lacks its command, KeepAlive or log, or names a store path";
              assert lib.assertMsg (
                builtins.elem "/home/user/.local/bin" (path true)
                && builtins.elem "/home/user/.local/bin" (path false)
                && builtins.elem "/opt/homebrew/bin" (path true)
                && !builtins.elem "/opt/homebrew/bin" (path false)
              ) "nix/module.nix: the holder PATH is not the profile, extraPath and the system directories";
              assert lib.assertMsg (
                (agent true a).config.EnvironmentVariables.TILLER_HOME == "/home/user/.tiller"
                && (envOf (unit false a)).TILLER_HOME == "/home/user/.tiller"
                && !(agent true a).config.EnvironmentVariables ? TILLER_PACKAGE
                && !(envOf (unit false a)) ? TILLER_PACKAGE
              ) "nix/module.nix: holder.environment does not reach the holder, or TILLER_PACKAGE does";
              pkgs.runCommand "tiller-module-holder-service" { } "touch $out";
          };

          devShells.default = craneLib.devShell {
            packages = [
              pkgs.bun
              pkgs.nodejs
              pkgs.cargo-nextest
              pkgs.sccache
              pkgs.sqlite
              bun2nixCli
            ]
            # Pixel diff for board screenshots, where nixpkgs builds it.
            ++ lib.optional (lib.meta.availableOn pkgs.stdenv.hostPlatform pkgs.odiff) pkgs.odiff;
            RUSTC_WRAPPER = "sccache";
            CARGO_INCREMENTAL = "0";
          };

          # The role behavior evals (evals/README.md): Claude Code at the
          # version evals/pins.env pins, from the locked nixpkgs. Claude Code
          # is unfree, so this shell allows that one package and no other.
          devShells.evals =
            let
              evalPkgs = import nixpkgs {
                inherit system;
                config.allowUnfreePredicate = p: lib.getName p == "claude-code";
              };
            in
            evalPkgs.mkShellNoCC {
              packages = [
                evalPkgs.claude-code
                evalPkgs.nodejs
                evalPkgs.git
                evalPkgs.bashInteractive
              ];
            };

          formatter = pkgs.nixfmt;
        };
    in
    {
      packages = forAllSystems (system: (perSystem system).packages);
      checks = forAllSystems (system: (perSystem system).checks);
      devShells = forAllSystems (system: (perSystem system).devShells);
      formatter = forAllSystems (system: (perSystem system).formatter);

      homeManagerModules.default = import ./nix/module.nix self;
    };
}
