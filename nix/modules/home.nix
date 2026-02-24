{ moduleWithSystem, ... }:
{
  flake.homeModules.default = moduleWithSystem (
    perSystem@{ self', ... }:
    home@{
      config,
      lib,
      pkgs,
      ...
    }:
    let
      cfg = config.services.orgmode-google-fuse;
    in
    {
      options = {
        services.orgmode-google-fuse = with lib; {
          enable = mkEnableOption ''
            Enable the orgmode-google-fuse service.
          '';

          package = mkOption {
            type = types.package;
            description = "orgmode-google-fuse derivation to use";
            default = self'.packages.orgmode-google-fuse;
          };

          mountPoint = mkOption {
            type = types.str;
            description = "Base mount point (relative to user state directory <literal>$XDG_STATE_HOME/orgmode-google-fuse<literal>).";
            default = "google";
          };

          systemdTarget = mkOption {
            type = types.str;
            default = "graphical-session.target";
            description = ''
              The systemd user unit which
              <literal>orgmode-google-fuse.service<literal> should be a part of.
            '';
          };
        };
      };

      config = lib.mkIf cfg.enable {
        home.packages = [ cfg.package ];

        systemd.user.services.orgmode-google-fuse = lib.mkIf pkgs.stdenv.isLinux {
          Unit = {
            Description = "orgmode-google-fuse";
            PartOf = [ cfg.systemdTarget ];
            After = [ cfg.systemdTarget ];
          };
          Install.WantedBy = [ cfg.systemdTarget ];
          Service = {
            ExecStart = ''
              ${pkgs.lib.getExe cfg.package} %S/orgmode-google-fuse/${cfg.mountPoint}
            '';
          };
        };

        launchd.agents.orgmode-google-fuse = lib.mkIf pkgs.stdenv.isDarwin {
          enable = true;
          config = {
            ProgramArguments = [
              "${pkgs.lib.getExe cfg.package}"
              "$HOME/Library/Application Support/orgmode-google-fuse/${cfg.mountPoint}"
            ];
            KeepAlive = true;
            RunAtLoad = true;
            StandardOutPath = "${config.home.homeDirectory}/Library/Logs/orgmode-google-fuse.log";
            StandardErrorPath = "${config.home.homeDirectory}/Library/Logs/orgmode-google-fuse.log";
          };
        };
      };
    }
  );
}
