{
  inputs,
  self,
  ...
}:
{
  imports = [
    inputs.rust-flake.flakeModules.default
    inputs.rust-flake.flakeModules.nixpkgs
  ];
  perSystem =
    {
      self',
      pkgs,
      lib,
      config,
      ...
    }:
    {
      rust-project = {
        crates."orgmode-google-fuse".crane.args = {
          buildInputs =
            lib.optionals pkgs.stdenv.isLinux [ pkgs.fuse3 ]
            ++ lib.optionals pkgs.stdenv.isDarwin [ pkgs.macfuse-stubs ];
          meta = {
            description = "Expose Google Calendar and Google Tasks as orgmode files using FUSE";
            homepage = "https://github.com/NickHu/orgmode-google-fuse";
            license = lib.licenses.mit;
            platforms = lib.platforms.linux ++ lib.platforms.darwin;
            mainProgram = "orgmode-google-fuse";
          };
        };
        src = lib.cleanSourceWith {
          src = self; # The original, unfiltered source
          filter =
            path: type:
            # make snapshot tests work
            (lib.hasSuffix ".org" path)
            || (lib.hasSuffix ".snap" path)
            ||
              # Default filter from crane (allow .rs files)
              (config.rust-project.crane-lib.filterCargoSources path type);
        };
      };
      packages.default = self'.packages.orgmode-google-fuse;
    };
}
