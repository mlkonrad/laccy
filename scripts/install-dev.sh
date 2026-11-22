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
install -Dm755 "$root/target/release/laccy-wayland" "$bin_dir/laccy-wayland"

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
ext_dir="$data_dir/gnome-shell/extensions/$uuid"
glib-compile-schemas "$root/extension/$uuid/schemas"
# A real folder is the extension from extensions.gnome.org; keep using it.
if [ -d "$ext_dir" ] && [ ! -L "$ext_dir" ]; then
    echo "Keeping the extension installed from extensions.gnome.org."
else
    ln -sfn "$root/extension/$uuid" "$ext_dir"
fi

# Restart the daemon on the new binary; D-Bus activation starts it again on the next copy.
pkill -x laccyd || true
# An open popup is a single instance that would keep answering the shortcut with the old binary.
pkill -x laccy || true
busctl --user call org.freedesktop.DBus /org/freedesktop/DBus org.freedesktop.DBus ReloadConfig >/dev/null 2>&1 || true
# laccyd only takes copies from the laccy-wayland file now installed, so an old one
# still running would be refused. Restart it from this terminal's Wayland session.
wayland_note=""
if pkill -x laccy-wayland; then
    setsid -f "$bin_dir/laccy-wayland" </dev/null >/dev/null 2>&1
    sleep 1
    if pgrep -x laccy-wayland >/dev/null; then
        wayland_note="laccy-wayland was restarted on the new binary."
    else
        wayland_note="laccy-wayland could not be restarted from here; log in again to start it."
    fi
fi

# Outside GNOME (Sway, Hyprland, Niri...), the compositor's config does the extension's job.
case ":${XDG_CURRENT_DESKTOP:-}:" in
    *:GNOME:*) gnome=1 ;;
    *) gnome="" ;;
esac

if [ -z "$gnome" ]; then
    cat <<EOF
Laccy installed. Outside GNOME, laccy-wayland does the extension's job.
In your compositor's config:
  1. Start $bin_dir/laccy-wayland with your session.
  2. Bind a key to $bin_dir/laccy.
  3. Make windows with the app ID io.github.mlkonrad.Laccy float.
Use these full paths: compositors often start programs without ~/.local/bin in PATH.
README.md has examples for Sway, Hyprland and Niri, under
"Sway, Hyprland, Niri and other Wayland compositors".
EOF
    # Without GNOME running, gnome-extensions changes the setting directly,
    # so the extension is on at the next GNOME login.
    if command -v gnome-extensions >/dev/null; then
        if gnome-extensions enable "$uuid" >/dev/null 2>&1; then
            echo "The GNOME extension is enabled too, for when you log in to GNOME."
        else
            echo "To use Laccy in GNOME too, log in there and run once: gnome-extensions enable $uuid"
        fi
    fi
elif gnome-extensions info "$uuid" >/dev/null 2>&1; then
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
if [ -n "$wayland_note" ]; then
    echo "$wayland_note"
fi
