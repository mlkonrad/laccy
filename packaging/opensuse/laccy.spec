# RPM spec for Laccy on openSUSE Tumbleweed, built in the Open Build
# Service (OBS). The Fedora spec for COPR is laccy.spec at the top of the
# repository.
#
# OBS builds without network access, so the crates come from a second
# tarball (vendor.tar.xz, made with cargo vendor). scripts/build-obs-source.sh
# makes both tarballs and sets Version from Cargo.toml. The changelog is in
# laccy.changes, which OBS adds as %changelog.

%global uuid laccy@mlkonrad.github.com

# The release profile strips the binaries, so there is no debug info to extract.
%global debug_package %{nil}

Name:           laccy
Version:        0.1.6
Release:        0
Summary:        Lightweight, keyboard-first clipboard manager for GNOME
# Laccy is MIT. The binaries also link crates from crates.io, which are
# under MIT, Apache-2.0 or both, and SQLite, which is public domain.
License:        MIT
Group:          Productivity/Other
URL:            https://github.com/mlkonrad/laccy
Source0:        %{url}/archive/v%{version}/%{name}-%{version}.tar.gz
Source1:        vendor.tar.xz
BuildRequires:  cargo >= 1.85
BuildRequires:  desktop-file-utils
BuildRequires:  gcc
# The tests compile the settings schema with glib-compile-schemas.
BuildRequires:  glib2-tools
BuildRequires:  pkgconfig
BuildRequires:  rust >= 1.85
BuildRequires:  pkgconfig(gtk4)
BuildRequires:  pkgconfig(libadwaita-1)
# The extension watches the clipboard and opens the popup; without it the
# app does nothing on GNOME. Weak, so people who install the extension from
# extensions.gnome.org can leave this copy out, and only where GNOME Shell
# is installed, so it doesn't pull GNOME onto Sway, Hyprland or Niri systems.
Recommends:     (gnome-shell-extension-laccy = %{version} if gnome-shell)

%description
Laccy is a lightweight, keyboard-first clipboard manager for GNOME.
It records text, images and files you copy, and opens a popup to search your
history and copy an item again.

This package has the popup and preferences (laccy), the history daemon
(laccyd) and laccy-wayland. On GNOME, the GNOME Shell extension in
gnome-shell-extension-laccy watches the clipboard. On Sway, Hyprland, Niri
and other Wayland compositors that support ext-data-control-v1,
laccy-wayland does that instead: start it with your session and bind a key
to laccy in the compositor's config; see README.md.

%package -n gnome-shell-extension-laccy
Summary:        GNOME Shell extension for the Laccy clipboard manager
Group:          System/GUI/GNOME
Requires:       %{name} = %{version}
Requires:       gnome-shell >= 50
BuildArch:      noarch

%description -n gnome-shell-extension-laccy
The GNOME Shell part of Laccy: it watches the clipboard, adds the top bar
icon and the shortcut, and puts items picked in the popup back on the
clipboard.

%prep
%autosetup -a1
mkdir -p .cargo
cat >.cargo/config.toml <<EOF
[source.crates-io]
replace-with = "vendored"

[source.vendored]
directory = "vendor"
EOF

%build
cargo build --release --locked --offline

%install
install -Dm755 -t %{buildroot}%{_bindir} \
    target/release/laccy target/release/laccyd target/release/laccy-wayland

sed "s|@BINDIR@|%{_bindir}|g" data/io.github.mlkonrad.Laccy.desktop.in \
    > io.github.mlkonrad.Laccy.desktop
desktop-file-install --dir=%{buildroot}%{_datadir}/applications io.github.mlkonrad.Laccy.desktop

mkdir -p %{buildroot}%{_datadir}/dbus-1/services
sed "s|@BINDIR@|%{_bindir}|g" data/io.github.mlkonrad.Laccy.Daemon.service.in \
    > %{buildroot}%{_datadir}/dbus-1/services/io.github.mlkonrad.Laccy.Daemon.service

install -Dm644 -t %{buildroot}%{_datadir}/icons/hicolor/scalable/apps \
    data/icons/hicolor/scalable/apps/io.github.mlkonrad.Laccy.svg

# The schema ships with the app, not the extension, so laccy-wayland finds
# it without GNOME. The glib2-tools file triggers compile it.
install -Dm644 -t %{buildroot}%{_datadir}/glib-2.0/schemas \
    extension/%{uuid}/schemas/org.gnome.shell.extensions.laccy.gschema.xml
install -Dm644 -t %{buildroot}%{_datadir}/gnome-shell/extensions/%{uuid} \
    extension/%{uuid}/extension.js extension/%{uuid}/metadata.json
install -Dm644 -t %{buildroot}%{_datadir}/gnome-shell/extensions/%{uuid}/lib \
    extension/%{uuid}/lib/*.js

%check
# The thumbnail tests decode images with glycin, which runs each decoder in
# a bwrap sandbox. Some build environments can't start one, so those tests
# only run where it works.
if bwrap --unshare-all --ro-bind / / --dev /dev true 2>/dev/null; then
    cargo test --release --locked --offline --workspace
else
    echo "bwrap can't start a sandbox here; skipping the thumbnail tests"
    cargo test --release --locked --offline --workspace -- --skip thumbnail::tests::
fi
desktop-file-validate %{buildroot}%{_datadir}/applications/io.github.mlkonrad.Laccy.desktop

%files
%license LICENSE
%doc README.md
%{_bindir}/laccy
%{_bindir}/laccyd
%{_bindir}/laccy-wayland
%{_datadir}/applications/io.github.mlkonrad.Laccy.desktop
%dir %{_datadir}/dbus-1
%dir %{_datadir}/dbus-1/services
%{_datadir}/dbus-1/services/io.github.mlkonrad.Laccy.Daemon.service
%{_datadir}/icons/hicolor/scalable/apps/io.github.mlkonrad.Laccy.svg
%{_datadir}/glib-2.0/schemas/org.gnome.shell.extensions.laccy.gschema.xml

%files -n gnome-shell-extension-laccy
%license LICENSE
%dir %{_datadir}/gnome-shell
%dir %{_datadir}/gnome-shell/extensions
%{_datadir}/gnome-shell/extensions/%{uuid}/

%changelog
