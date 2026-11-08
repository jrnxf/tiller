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
      systems = [
        "aarch64-darwin"
        "x86_64-darwin"
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
              # Claude Code viewport reads the composer classifier is tested on.
              ./crates/herdr/tests/fixtures
              # Embedded by crates/agents with include_str!.
              ./roles
              # Embedded by crates/replay with include_str!.
              ./crates/replay/prompts
              # The role matrix core::authority::matrix_matches_spec reads with include_str!.
              ./docs/spec/authority.md
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
                        map (lib.removeSuffix "\"" ) (map (lib.removePrefix "\"") service.Service.Environment)
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
              assert lib.assertMsg (!hasDir false "/opt/homebrew/bin")
                "nix/module.nix: the systemd unit's PATH has /opt/homebrew/bin, which only macOS has";
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
          };

          devShells.default = craneLib.devShell {
            packages = [
              pkgs.bun
              pkgs.nodejs
              pkgs.cargo-nextest
              pkgs.sccache
              pkgs.sqlite
              bun2nixCli
            ];
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
