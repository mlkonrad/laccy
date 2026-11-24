# RPM spec for Laccy, built in COPR by Packit (see .packit.yaml).
#
# Crates are fetched from crates.io during the build (Cargo.lock pins them),
# so the COPR project needs network access during builds turned on.

%global uuid laccy@mlkonrad.github.com

# The release profile strips the binaries, so there is no debug info to extract.
%global debug_package %{nil}

Name:           laccy
Version:        0.1.4
Release:        1%{?dist}
Summary:        Lightweight, keyboard-first clipboard manager for GNOME

# Laccy is MIT. The binaries also link crates from crates.io, which are
# under MIT, Apache-2.0 or both, and SQLite, which is public domain.
License:        MIT
URL:            https://github.com/mlkonrad/laccy
Source0:        %{url}/archive/v%{version}/%{name}-%{version}.tar.gz

BuildRequires:  cargo >= 1.85
BuildRequires:  rust >= 1.85
BuildRequires:  gcc
BuildRequires:  pkgconfig(gtk4)
BuildRequires:  pkgconfig(libadwaita-1)
BuildRequires:  desktop-file-utils
# The tests compile the settings schema.
BuildRequires:  /usr/bin/glib-compile-schemas

# The extension watches the clipboard and opens the popup; without it the
# app does nothing on GNOME. Weak, so people who install the extension from
# extensions.gnome.org can leave this copy out, and only where GNOME Shell
# is installed, so it doesn't pull GNOME onto Sway, Hyprland or Niri systems.
Recommends:     (gnome-shell-extension-laccy = %{version}-%{release} if gnome-shell)

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
BuildArch:      noarch
Requires:       gnome-shell >= 50
Requires:       %{name} = %{version}-%{release}

%description -n gnome-shell-extension-laccy
The GNOME Shell part of Laccy: it watches the clipboard, adds the top bar
icon and the shortcut, and puts items picked in the popup back on the
clipboard.

%prep
%autosetup -n %{name}-%{version}

%build
cargo build --release --locked

%install
install -Dm755 target/release/laccy %{buildroot}%{_bindir}/laccy
install -Dm755 target/release/laccyd %{buildroot}%{_bindir}/laccyd
install -Dm755 target/release/laccy-wayland %{buildroot}%{_bindir}/laccy-wayland

sed "s|@BINDIR@|%{_bindir}|g" data/io.github.mlkonrad.Laccy.desktop.in \
    > io.github.mlkonrad.Laccy.desktop
desktop-file-install --dir=%{buildroot}%{_datadir}/applications io.github.mlkonrad.Laccy.desktop

mkdir -p %{buildroot}%{_datadir}/dbus-1/services
sed "s|@BINDIR@|%{_bindir}|g" data/io.github.mlkonrad.Laccy.Daemon.service.in \
    > %{buildroot}%{_datadir}/dbus-1/services/io.github.mlkonrad.Laccy.Daemon.service

install -Dm644 data/icons/hicolor/scalable/apps/io.github.mlkonrad.Laccy.svg \
    %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/io.github.mlkonrad.Laccy.svg

# The schema goes to the system schema folder, where both the extension and
# laccy find it; Fedora's glib2 file triggers compile it.
install -Dm644 extension/%{uuid}/extension.js %{buildroot}%{_datadir}/gnome-shell/extensions/%{uuid}/extension.js
install -Dm644 -t %{buildroot}%{_datadir}/gnome-shell/extensions/%{uuid}/lib extension/%{uuid}/lib/*.js
install -Dm644 extension/%{uuid}/metadata.json %{buildroot}%{_datadir}/gnome-shell/extensions/%{uuid}/metadata.json
install -Dm644 extension/%{uuid}/schemas/org.gnome.shell.extensions.laccy.gschema.xml \
    %{buildroot}%{_datadir}/glib-2.0/schemas/org.gnome.shell.extensions.laccy.gschema.xml

%check
cargo test --release --locked --workspace
desktop-file-validate %{buildroot}%{_datadir}/applications/io.github.mlkonrad.Laccy.desktop

%files
%license LICENSE
%doc README.md
%{_bindir}/laccy
%{_bindir}/laccyd
# Small and idle unless a compositor's config starts it, so it ships here
# rather than in a package people on other compositors would have to find.
%{_bindir}/laccy-wayland
%{_datadir}/applications/io.github.mlkonrad.Laccy.desktop
%{_datadir}/dbus-1/services/io.github.mlkonrad.Laccy.Daemon.service
%{_datadir}/icons/hicolor/scalable/apps/io.github.mlkonrad.Laccy.svg
# Here, not in the extension's package, so laccy-wayland finds it without GNOME.
%{_datadir}/glib-2.0/schemas/org.gnome.shell.extensions.laccy.gschema.xml

%files -n gnome-shell-extension-laccy
%license LICENSE
%{_datadir}/gnome-shell/extensions/%{uuid}/

%changelog
* Tue Nov 24 2026 Marlon Konrad <mlkonrad@users.noreply.github.com> - 0.1.4-1
- Add laccy-wayland for Sway, Hyprland, Niri and other Wayland compositors (experimental)
- Ship laccy-wayland and the settings schema in the laccy package
- Recommend the extension only where GNOME Shell is installed

* Sat Nov 14 2026 Marlon Konrad <mlkonrad@users.noreply.github.com> - 0.1.3-1
- Clean up the extension's indicator in destroy() for the extensions.gnome.org review

* Sat Nov 14 2026 Marlon Konrad <mlkonrad@users.noreply.github.com> - 0.1.2-1
- Split the extension into modules for the extensions.gnome.org review
- Rename the extension's D-Bus interface to org.gnome.Shell.Extensions.Laccy

* Sat Nov 14 2026 Marlon Konrad <mlkonrad@users.noreply.github.com> - 0.1.1-1
- Update the preview when a search changes the selected item
- Disconnect the extension's dialog handlers when it is disabled

* Fri Nov 13 2026 Marlon Konrad <mlkonrad@users.noreply.github.com> - 0.1.0-1
- First package
