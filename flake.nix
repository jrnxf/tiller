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
          boardCommon = {
            pname = "tiller-board";
            version = "0.1.0";
            src = boardSrc;
            # vite and vitest run on real node: bun's node shim cannot run jsdom.
            nativeBuildInputs = [
              pkgs.bun2nix.hook
              pkgs.nodejs
            ];
            bunDeps = pkgs.bun2nix.fetchBunDeps {
              bunNix = ./board/bun.nix;
              useFakeNode = false;
            };
            dontRunLifecycleScripts = true;
          };
          board = pkgs.stdenv.mkDerivation (
            boardCommon
            // {
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
            }
          );
          boardChecks = pkgs.stdenv.mkDerivation (
            boardCommon
            // {
              pname = "tiller-board-checks";
              buildPhase = ''
                runHook preBuild
                bun run typecheck
                bun run lint
                bun run format:check
                bun run knip
                bun run test
                runHook postBuild
              '';
              installPhase = ''
                touch $out
              '';
            }
          );

          # The tiller binary, with the board's static files embedded.
          src = lib.fileset.toSource {
            root = ./.;
            fileset = lib.fileset.unions [
              (craneLib.fileset.commonCargoSources ./.)
              ./crates/cli/tests/fixtures
            ];
          };
          commonArgs = {
            inherit src;
            pname = "tiller";
            version = (fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;
            strictDeps = true;
          };
          cargoArtifacts = craneLib.buildDepsOnly commonArgs;
          tiller = craneLib.buildPackage (
            commonArgs
            // {
              inherit cargoArtifacts;
              cargoExtraArgs = "--package tiller-cli";
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
            inherit tiller boardChecks;
            clippy = craneLib.cargoClippy (
              commonArgs
              // {
                inherit cargoArtifacts;
                cargoClippyExtraArgs = "--workspace --all-targets -- -D warnings";
              }
            );
            fmt = craneLib.cargoFmt { inherit src; };
            nextest = craneLib.cargoNextest (commonArgs // { inherit cargoArtifacts; });
            smoke =
              pkgs.runCommand "tiller-smoke"
                {
                  nativeBuildInputs = [ pkgs.curl ];
                  # The smoke test binds a localhost port inside the sandbox.
                  __darwinAllowLocalNetworking = true;
                }
                ''
                  bash ${./scripts/smoke.sh} ${tiller}/bin/tiller
                  touch $out
                '';
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
