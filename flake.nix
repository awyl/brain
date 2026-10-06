{
  description = "brain: a second brain server over a markdown vault";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url = "github:numtide/flake-utils";

  };

  outputs = {
    nixpkgs,
    rust-overlay,
    flake-utils,
    ...
  }:
    flake-utils.lib.eachDefaultSystem (
      system: let
        overlays = [
          (import rust-overlay)
        ];
        pkgs = import nixpkgs {
          inherit system overlays;
          config.allowUnfree = true;
        };
      in
        with pkgs; {
          # The binary, with git (vault), curl (brain fetch) and cloudflared (brain tunnel) on its PATH.
          # Home Manager / NixOS: add `inputs.brain.packages.${system}.default` to home.packages / environment.systemPackages.
          packages.default = rustPlatform.buildRustPackage {
            pname = "brain";
            version = "0.1.0";
            src = ./.;
            cargoLock.lockFile = ./Cargo.lock;
            nativeBuildInputs = [makeWrapper];
            postInstall = ''
              wrapProgram $out/bin/brain --prefix PATH : ${lib.makeBinPath [git curl cloudflared]}
            '';
          };

          devShells.default = mkShell {
            nativeBuildInputs = [
              openssl
              # gcc12
              pkg-config
            ];
            buildInputs = [
              (rust-bin.stable.latest.default.override
                {
                  extensions = ["rust-src" "rust-analyzer"];
                  targets = [ "wasm32-unknown-unknown" ];

                })

              mold

              # cache build
              sccache

              jq
              hurl

            ];

            # for gui to run on linux wayland
            env.RUSTFLAGS = "-C link-arg=-fuse-ld=mold";
            # for caching build
            env.RUSTC_WRAPPER = "${pkgs.sccache.outPath}/bin/sccache";
          };
        }
    );
}
