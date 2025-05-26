{ inputs, ... }:
{
  imports = [
    inputs.rust-flake.flakeModules.default
    inputs.rust-flake.flakeModules.nixpkgs
  ];
  perSystem =
    { self', pkgs, ... }:
    {
      rust-project.crates."orgmode-google-fuse".crane.args = {
        buildInputs = [ pkgs.fuse3 ];
      };
      packages.default = self'.packages.orgmode-google-fuse;
    };
}
