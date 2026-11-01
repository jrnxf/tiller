# Home-manager module stub. The launchd agent and systemd user unit arrive
# with the daemon; for now it only installs the package.
self:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.programs.tiller;
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
  };

  config = lib.mkIf cfg.enable {
    home.packages = [ cfg.package ];
  };
}
