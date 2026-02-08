{ inputs, ... }:
{
  imports = [
    inputs.flake-parts.flakeModules.easyOverlay
  ];
  perSystem =
    { config, ... }:
    {
      overlayAttrs = {
        inherit (config.packages) orgmode-google-fuse;
      };
    };
}
