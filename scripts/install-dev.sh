#!/usr/bin/env bash
# Builds Laccy and installs it for the current user. No root needed.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
bin_dir="$HOME/.local/bin"
data_dir="${XDG_DATA_HOME:-$HOME/.local/share}"
uuid="laccy@laccy.github.io"

cargo build --release --manifest-path "$root/Cargo.toml"
install -Dm755 "$root/target/release/laccy" "$bin_dir/laccy"
install -Dm755 "$root/target/release/laccyd" "$bin_dir/laccyd"

mkdir -p "$data_dir/applications" "$data_dir/dbus-1/services" "$data_dir/gnome-shell/extensions"
sed "s|@BINDIR@|$bin_dir|g" "$root/data/io.github.laccy.Laccy.desktop.in" \
    > "$data_dir/applications/io.github.laccy.Laccy.desktop"
sed "s|@BINDIR@|$bin_dir|g" "$root/data/io.github.laccy.Laccy.Daemon.service.in" \
    > "$data_dir/dbus-1/services/io.github.laccy.Laccy.Daemon.service"

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
    echo "Laccy installed and extension enabled. Press Shift+Super+C."
else
    cat <<EOF
Laccy installed. GNOME Shell only discovers new extensions at login, so:
  1. Log out and back in.
  2. Run: gnome-extensions enable $uuid
  3. Press Shift+Super+C.
EOF
fi
