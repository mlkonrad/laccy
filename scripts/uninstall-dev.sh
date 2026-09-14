#!/usr/bin/env bash
# Removes what install-dev.sh installed. Clipboard history in
# ~/.local/share/laccy and config in ~/.config/laccy are kept.
set -euo pipefail

bin_dir="$HOME/.local/bin"
data_dir="${XDG_DATA_HOME:-$HOME/.local/share}"
uuid="laccy@mlkonrad.github.com"

gnome-extensions disable "$uuid" 2>/dev/null || true
pkill -x laccyd || true

rm -f "$bin_dir/laccy" "$bin_dir/laccyd" \
    "$data_dir/applications/io.github.mlkonrad.Laccy.desktop" \
    "$data_dir/dbus-1/services/io.github.mlkonrad.Laccy.Daemon.service"
rm -f "$data_dir/gnome-shell/extensions/$uuid"

echo "Laccy uninstalled. Delete ~/.local/share/laccy to remove the clipboard history."
