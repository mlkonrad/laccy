# Nix flake for Laccy: the app (laccy, laccyd and laccy-wayland) and the
# GNOME Shell extension, as two packages. See README.md, "NixOS".
#
# The binaries are not wrapped in launcher scripts as most GTK apps on Nix
# are: laccyd, laccy-wayland and the extension only accept D-Bus calls from
# processes running the real laccy binaries (compared by inode), and a
# wrapper would run a renamed copy. LACCY_SCHEMA_DIR tells the app where
# its settings schema is instead.
{
  description = "Lightweight, keyboard-first clipboard manager for GNOME and Wayland compositors";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      lib = nixpkgs.lib;
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = f: lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
      version = (lib.importTOML ./Cargo.toml).workspace.package.version;
      uuid = "laccy@mlkonrad.github.com";
      meta = {
        homepage = "https://github.com/mlkonrad/laccy";
        license = lib.licenses.mit;
        platforms = lib.platforms.linux;
      };
    in
    {
      packages = forAllSystems (pkgs: rec {
        default = laccy;

        laccy = pkgs.rustPlatform.buildRustPackage {
          pname = "laccy";
          inherit version;

          # Only what the build reads, so edits elsewhere don't rebuild it.
          src = lib.fileset.toSource {
            root = ./.;
            fileset = lib.fileset.unions [
              ./Cargo.toml
              ./Cargo.lock
              ./crates
              ./data/icons
              ./data/io.github.mlkonrad.Laccy.desktop.in
              ./data/io.github.mlkonrad.Laccy.Daemon.service.in
              ./extension/${uuid}/schemas/org.gnome.shell.extensions.laccy.gschema.xml
              ./LICENSE
              ./README.md
            ];
          };
          cargoLock.lockFile = ./Cargo.lock;

          nativeBuildInputs = with pkgs; [
            pkg-config
            glib # glib-compile-schemas, also used by the tests
            desktop-file-utils
          ];
          buildInputs = with pkgs; [
            gtk4
            libadwaita
          ];

          env.LACCY_SCHEMA_DIR = "${placeholder "out"}/share/laccy/schemas";

          # nixpkgs' Rust setup makes rusqlite link the system SQLite. Use the
          # bundled copy, as the other packages do; this runs after that setup.
          preBuild = ''
            export LIBSQLITE3_SYS_USE_PKG_CONFIG=0
          '';

          # The thumbnail tests decode images with glycin, which needs a
          # bwrap sandbox, and the Nix build sandbox can't start one.
          checkFlags = [ "--skip=thumbnail::tests::" ];

          postInstall = ''
            mkdir -p $out/share/applications $out/share/dbus-1/services
            substitute data/io.github.mlkonrad.Laccy.desktop.in \
              $out/share/applications/io.github.mlkonrad.Laccy.desktop \
              --replace-fail @BINDIR@ $out/bin
            desktop-file-validate $out/share/applications/io.github.mlkonrad.Laccy.desktop
            substitute data/io.github.mlkonrad.Laccy.Daemon.service.in \
              $out/share/dbus-1/services/io.github.mlkonrad.Laccy.Daemon.service \
              --replace-fail @BINDIR@ $out/bin
            install -Dm644 -t $out/share/icons/hicolor/scalable/apps \
              data/icons/hicolor/scalable/apps/io.github.mlkonrad.Laccy.svg
            install -Dm644 -t $LACCY_SCHEMA_DIR \
              extension/${uuid}/schemas/org.gnome.shell.extensions.laccy.gschema.xml
            glib-compile-schemas $LACCY_SCHEMA_DIR
            install -Dm644 -t $out/share/doc/laccy README.md
          '';

          meta = meta // {
            description = "Lightweight, keyboard-first clipboard manager for GNOME and Wayland compositors";
            mainProgram = "laccy";
          };
        };

        gnome-shell-extension-laccy = pkgs.stdenvNoCC.mkDerivation {
          pname = "gnome-shell-extension-laccy";
          inherit version;
          src = lib.fileset.toSource {
            root = ./extension/${uuid};
            fileset = lib.fileset.unions [
              ./extension/${uuid}/extension.js
              ./extension/${uuid}/metadata.json
              ./extension/${uuid}/lib
              ./extension/${uuid}/schemas/org.gnome.shell.extensions.laccy.gschema.xml
            ];
          };
          nativeBuildInputs = [ pkgs.glib ];
          installPhase = ''
            dir=$out/share/gnome-shell/extensions/${uuid}
            mkdir -p $dir
            cp -r extension.js metadata.json lib schemas $dir/
            glib-compile-schemas $dir/schemas
          '';
          passthru.extensionUuid = uuid;
          meta = meta // {
            description = "GNOME Shell extension for the Laccy clipboard manager";
          };
        };
      });

      overlays.default = final: prev: {
        laccy = self.packages.${final.stdenv.hostPlatform.system}.laccy;
        gnome-shell-extension-laccy =
          self.packages.${final.stdenv.hostPlatform.system}.gnome-shell-extension-laccy;
      };
    };
}
