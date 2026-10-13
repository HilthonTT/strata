{
  description = "strata — a fast, extensible terminal file explorer";

  # strata needs Rust 1.95 or newer; nixpkgs-unstable tracks recent releases.
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        manifest = (pkgs.lib.importTOML ./Cargo.toml).workspace.package;
      in
      {
        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "strata";
          inherit (manifest) version;
          src = pkgs.lib.cleanSource ./.;
          # Every dependency comes from crates.io, so the lock file is enough
          # (no outputHashes needed).
          cargoLock.lockFile = ./Cargo.lock;
          cargoBuildFlags = [ "-p" "strata" ];

          nativeBuildInputs = [ pkgs.pkg-config ];
          buildInputs = [ pkgs.openssl pkgs.zlib ];
          env.OPENSSL_NO_VENDOR = "1";

          # The tests run in CI; some expect git and a writable home.
          doCheck = false;

          meta = {
            description = "A fast, extensible terminal file explorer";
            homepage = "https://github.com/HilthonTT/strata";
            license = pkgs.lib.licenses.mit;
            mainProgram = "strata";
          };
        };

        apps.default = flake-utils.lib.mkApp { drv = self.packages.${system}.default; };

        devShells.default = pkgs.mkShell {
          inputsFrom = [ self.packages.${system}.default ];
          packages = with pkgs; [ cargo rustc rustfmt clippy rust-analyzer gnumake git tmux ];
        };
      });
}
