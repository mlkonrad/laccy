#!/usr/bin/env bash
# Removes what install-dev.sh installed. The clipboard history in
# ~/.local/share/laccy and the settings in dconf are kept.
set -euo pipefail

bin_dir="$HOME/.local/bin"
data_dir="${XDG_DATA_HOME:-$HOME/.local/share}"
uuid="laccy@mlkonrad.github.com"

ext_dir="$data_dir/gnome-shell/extensions/$uuid"
# Only the link install-dev.sh made; an extension from extensions.gnome.org stays.
if [ -L "$ext_dir" ]; then
    gnome-extensions disable "$uuid" 2>/dev/null || true
    rm -f "$ext_dir"
fi
pkill -x laccyd || true

rm -f "$bin_dir/laccy" "$bin_dir/laccyd" \
    "$data_dir/applications/io.github.mlkonrad.Laccy.desktop" \
    "$data_dir/dbus-1/services/io.github.mlkonrad.Laccy.Daemon.service" \
    "$data_dir/icons/hicolor/scalable/apps/io.github.mlkonrad.Laccy.svg"

echo "Laccy uninstalled. Your history and settings are kept. To remove them too:"
echo "  rm -r ~/.local/share/laccy"
echo "  dconf reset -f /org/gnome/shell/extensions/laccy/"
