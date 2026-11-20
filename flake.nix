{
  description = "bligle: Eagle schematic and board viewer, side by side and cross-probed (Blit)";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      # Blit's desktop backend is Wayland only on Linux
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});

      # Blit loads these with dlopen: the Wayland window, its keymap, and fontconfig
      # (its parley text backend finds the system fallback fonts with it)
      runtimeLibs =
        pkgs: with pkgs; [
          wayland
          libxkbcommon
          fontconfig
        ];

      package =
        {
          lib,
          rustPlatform,
          resvg,
          pkgs,
        }:
        rustPlatform.buildRustPackage {
          pname = "bligle";
          version = (lib.importTOML ./Cargo.toml).package.version;

          src = lib.fileset.toSource {
            root = ./.;
            fileset = lib.fileset.unions [
              ./Cargo.toml
              ./Cargo.lock
              ./.cargo
              ./src
              ./assets
            ];
          };

          cargoLock = {
            lockFile = ./Cargo.lock;
            # the blit crates are fetched at the revision of Cargo.lock, so updating
            # Blit needs no hash here
            allowBuiltinFetchGit = true;
          };

          # Blit uses nightly-only features (see .cargo/config.toml)
          env.RUSTC_BOOTSTRAP = 1;

          nativeBuildInputs = [ resvg ];

          # the unit tests run (the one reading ../Projects skips: it is not in the repository)

          postInstall = ''
            install -Dm644 assets/bligle.desktop $out/share/applications/bligle.desktop
            install -Dm644 assets/bligle-mime.xml $out/share/mime/packages/bligle.xml
            install -Dm644 assets/bligle.svg $out/share/icons/hicolor/scalable/apps/bligle.svg
            for size in 16 24 32 48 64 128 256 512; do
              mkdir -p $out/share/icons/hicolor/''${size}x$size/apps
              resvg -w $size -h $size assets/bligle.svg $out/share/icons/hicolor/''${size}x$size/apps/bligle.png
            done
          '';

          # The libraries go in the binary's runpath (not in DT_NEEDED: they are
          # dlopened), added after the fixup phase which would shrink them away.
          postFixup = ''
            patchelf --add-rpath ${lib.makeLibraryPath (runtimeLibs pkgs)} $out/bin/bligle
          '';

          meta = {
            description = "Eagle schematic and board viewer, side by side and cross-probed, with a headless mode for agents";
            homepage = "https://github.com/GPTechinno/bligle";
            mainProgram = "bligle";
            platforms = systems;
          };
        };
    in
    {
      packages = forAllSystems (pkgs: rec {
        bligle = pkgs.callPackage package { };
        default = bligle;
      });

      # nixpkgs.overlays = [ bligle.overlays.default ]; then pkgs.bligle
      overlays.default = final: _: { bligle = final.callPackage package { }; };

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [
            cargo
            rustc
            clippy
            rustfmt
            rust-analyzer
            resvg
          ];
          RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (runtimeLibs pkgs);
        };
      });

      formatter = forAllSystems (pkgs: pkgs.nixfmt);
    };
}
