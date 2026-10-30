{
  description = "Nixblend - Package manager for NixOS";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";

  outputs = { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
    in
    {
      packages.${system}.default = pkgs.rustPlatform.buildRustPackage {
        pname = "nixblend";
        version = "0.1.0";

        src = self;

        cargoLock.lockFile = ./Cargo.lock;

        meta = {
          description = "A package manager for NixOS";
          mainProgram = "nixblend";
          platforms = pkgs.lib.platforms.linux;
        };
      };
    };
}
