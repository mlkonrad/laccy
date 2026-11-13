#!/usr/bin/env bash
# Builds Laccy and installs it for the current user. No root needed.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
bin_dir="$HOME/.local/bin"
data_dir="${XDG_DATA_HOME:-$HOME/.local/share}"
uuid="laccy@mlkonrad.github.com"

cargo build --release --manifest-path "$root/Cargo.toml"
install -Dm755 "$root/target/release/laccy" "$bin_dir/laccy"
install -Dm755 "$root/target/release/laccyd" "$bin_dir/laccyd"

mkdir -p "$data_dir/applications" "$data_dir/dbus-1/services" "$data_dir/gnome-shell/extensions"
sed "s|@BINDIR@|$bin_dir|g" "$root/data/io.github.mlkonrad.Laccy.desktop.in" \
    > "$data_dir/applications/io.github.mlkonrad.Laccy.desktop"
sed "s|@BINDIR@|$bin_dir|g" "$root/data/io.github.mlkonrad.Laccy.Daemon.service.in" \
    > "$data_dir/dbus-1/services/io.github.mlkonrad.Laccy.Daemon.service"
install -Dm644 "$root/data/icons/hicolor/scalable/apps/io.github.mlkonrad.Laccy.svg" \
    "$data_dir/icons/hicolor/scalable/apps/io.github.mlkonrad.Laccy.svg"
# Refresh the user icon cache if one exists, or the new icon stays hidden behind it.
if [ -f "$data_dir/icons/hicolor/icon-theme.cache" ]; then
    gtk4-update-icon-cache -qtf "$data_dir/icons/hicolor" 2>/dev/null \
        || gtk-update-icon-cache -qtf "$data_dir/icons/hicolor" 2>/dev/null || true
fi

# The extension is symlinked, so edits to its JS apply after re-logging in.
glib-compile-schemas "$root/extension/$uuid/schemas"
ln -sfn "$root/extension/$uuid" "$data_dir/gnome-shell/extensions/$uuid"

# Restart the daemon on the new binary; D-Bus activation starts it again on the next copy.
pkill -x laccyd || true
# An open popup is a single instance that would keep answering the shortcut with the old binary.
pkill -x laccy || true
busctl --user call org.freedesktop.DBus /org/freedesktop/DBus org.freedesktop.DBus ReloadConfig >/dev/null 2>&1 || true

if gnome-extensions info "$uuid" >/dev/null 2>&1; then
    gnome-extensions enable "$uuid"
    echo "Laccy installed and extension enabled. Click the clipboard icon in the top bar, or set a shortcut in Preferences."
else
    cat <<EOF
Laccy installed. GNOME Shell only discovers new extensions at login, so:
  1. Log out and back in.
  2. Run: gnome-extensions enable $uuid
  3. Click the clipboard icon in the top bar, or set a shortcut in Preferences.
EOF
fi
