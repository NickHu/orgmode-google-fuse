{ inputs, ... }:
{
  imports = [
    inputs.rust-flake.flakeModules.default
    inputs.rust-flake.flakeModules.nixpkgs
  ];
  perSystem =
    { self'
    , pkgs
    , lib
    , ...
    }:
    {
      rust-project.crates."orgmode-google-fuse".crane.args = {
        buildInputs =
          lib.optionals pkgs.stdenv.isLinux [ pkgs.fuse3 ]
          ++ lib.optionals pkgs.stdenv.isDarwin [ pkgs.macfuse-stubs ];
      };
      packages.default = self'.packages.orgmode-google-fuse;
    };
}
