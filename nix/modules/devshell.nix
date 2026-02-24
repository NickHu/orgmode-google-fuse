{ inputs, ... }:
{
  perSystem =
    {
      config,
      self',
      pkgs,
      lib,
      ...
    }:
    {
      devShells.default = pkgs.mkShell {
        name = "orgmode-google-fuse-shell";
        inputsFrom = [
          self'.devShells.rust
          config.pre-commit.devShell # See ./nix/modules/pre-commit.nix
        ];
        packages = with pkgs; [
          cargo-insta
          nixd # Nix language server
        ];
      };
    };
}
