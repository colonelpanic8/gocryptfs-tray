{
  description = "System tray and CLI for gocryptfs vaults, with PIN-gated unlocking";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = {
    self,
    nixpkgs,
    fenix,
    flake-utils,
  }: let
    cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
  in
    flake-utils.lib.eachDefaultSystem (
      system: let
        pkgs = nixpkgs.legacyPackages.${system};
        toolchain = fenix.packages.${system}.stable.withComponents [
          "cargo"
          "clippy"
          "rust-src"
          "rustc"
          "rustfmt"
          "rust-analyzer"
        ];
        rustPlatform = pkgs.makeRustPlatform {
          cargo = toolchain;
          rustc = toolchain;
        };
        package = rustPlatform.buildRustPackage {
          pname = cargoToml.package.name;
          inherit (cargoToml.package) version;
          src = pkgs.lib.cleanSource ./.;
          cargoLock.lockFile = ./Cargo.lock;
          nativeBuildInputs = [pkgs.makeWrapper];
          postInstall = ''
            wrapProgram $out/bin/gocryptfs-tray \
              --suffix PATH : ${pkgs.lib.makeBinPath [pkgs.gocryptfs pkgs.libnotify pkgs.xdg-utils]}
          '';
          meta = {
            inherit (cargoToml.package) description;
            homepage = cargoToml.package.repository;
            license = with pkgs.lib.licenses; [mit asl20];
            mainProgram = "gocryptfs-tray";
            platforms = pkgs.lib.platforms.linux;
          };
        };
      in {
        packages.default = package;

        checks =
          {
            inherit package;
          }
          // pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
            vm = import ./nix/vm-test.nix {
              inherit pkgs;
              module = self.nixosModules.default;
            };
            module-eval =
              (nixpkgs.lib.nixosSystem {
                inherit system;
                modules = [
                  self.nixosModules.default
                  {
                    boot.loader.grub.enable = false;
                    fileSystems."/" = {
                      device = "nodev";
                      fsType = "tmpfs";
                    };
                    system.stateVersion = "25.11";
                    users.users.alice = {
                      isNormalUser = true;
                      group = "users";
                    };
                    users.users.syncthing = {
                      isSystemUser = true;
                      group = "syncthing";
                    };
                    users.groups.syncthing = {};
                    services.gocryptfs-tray.vaults.private = {
                      user = "alice";
                      cipherUser = "syncthing";
                      cipherDir = "/var/lib/syncthing/sync/Private.encrypted";
                      link = "/home/alice/Private";
                      secretCommand = ["pass" "show" "vault"];
                      syncthingIgnore.folder = "/var/lib/syncthing/sync";
                      autoLockMinutes = 30;
                    };
                  }
                ];
              }).config.system.build.toplevel;
          };

        devShells.default = pkgs.mkShell {
          packages = [
            toolchain
            pkgs.just
            pkgs.alejandra
            pkgs.gocryptfs
          ];
        };

        formatter = pkgs.alejandra;
      }
    )
    // {
      nixosModules.default = {pkgs, ...}: {
        imports = [./nix/module.nix];
        services.gocryptfs-tray.package = nixpkgs.lib.mkDefault self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      };
    };
}
