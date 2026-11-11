{
  description = "Apple Mail CLI that reads the local Mail store and acts through Mail.app";

  inputs.nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      inherit (nixpkgs) lib;
      systems = [
        "aarch64-darwin"
      ];
      mkPackage =
        pkgs:
        pkgs.rustPlatform.buildRustPackage {
          pname = "mailctl";
          inherit ((lib.importTOML ./Cargo.toml).package) version;

          src = ./.;
          # Dependencies come from Cargo.lock, so there is no vendor hash to maintain.
          cargoLock.lockFile = ./Cargo.lock;
          # rusqlite links the system SQLite, which the Nix sandbox does not provide.
          buildInputs = [ pkgs.sqlite ];
          # The commit this flake was built from, embedded only for the version report.
          MAILCTL_GIT_COMMIT = self.rev or self.dirtyRev or "unknown";

          meta = {
            description = "Apple Mail CLI that reads the local Mail store and acts through Mail.app";
            homepage = "https://github.com/zydtiger/mailctl";
            license = lib.licenses.mit;
            platforms = systems;
            mainProgram = "mailctl";
          };
        };
    in
    {
      packages = lib.genAttrs systems (
        system:
        let
          mailctl = mkPackage nixpkgs.legacyPackages.${system};
        in
        {
          inherit mailctl;
          default = mailctl;
        }
      );
    };
}
