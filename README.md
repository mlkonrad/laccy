# Laccy

A lightweight, keyboard-first clipboard manager for GNOME, inspired by
[Maccy](https://github.com/p0deje/Maccy). Press **Shift+Super+C**, type to
search, press **Enter** to paste.

Laccy is an independent project. It is not affiliated with Maccy and doesn't
share any code with it.

## Status

Early scaffold. It records and pastes text on GNOME 50 (Wayland).
See the [roadmap](#roadmap).

## How it works

GNOME doesn't let regular apps watch the clipboard, register global shortcuts
or type keys. Laccy is therefore three small parts:

```
 GNOME Shell extension (JS, runs inside gnome-shell)
   ├─ clipboard changed ──AddText──▶ laccyd (Rust, no UI)
   │                                   └─ SQLite ~/.local/share/laccy/history.db
   ├─ Shift+Super+C ──launches──▶ laccy popup (Rust, GTK 4 + libadwaita)
   │                                   └─ reads the same database
   └─ ◀──SetClipboard(text, paste)── laccy, when an item is picked
        sets the clipboard, then sends Ctrl+V
```

| Part | Where | Role |
|---|---|---|
| `extension/laccy@laccy.github.io` | inside gnome-shell | Watches the clipboard, handles the shortcut, sets the clipboard and pastes |
| `crates/laccyd` | background, D-Bus activated | Stores copied text; only glib and SQLite, no GTK |
| `crates/laccy` | runs only while visible | The popup: search, pick, pin, delete |
| `crates/laccy-core` | library | History database, search, config, D-Bus names |

**Keeping it lightweight:** no GTK process runs while the popup is hidden.
`laccy` starts on the shortcut and exits when it closes, `laccyd` is the only
thing that stays running, and the popup loads only a 10,000-character excerpt
of each item.

**Privacy:** copies marked as secret by password managers
(`x-kde-passwordManagerHint`) are never recorded, and you can ignore whole
apps (see [Configuration](#configuration)). Nothing is recorded while the
screen is locked, because GNOME disables extensions then. History never
leaves your machine.

## Build and install

Requirements (Fedora): `sudo dnf install cargo rust gtk4-devel libadwaita-devel`

```sh
scripts/install-dev.sh     # builds, installs into ~/.local, links the extension
```

The first time, log out and back in (GNOME Shell only discovers new extensions
at login), then run `gnome-extensions enable laccy@laccy.github.io`.

`scripts/uninstall-dev.sh` removes everything except your history.

Development:

```sh
cargo test --workspace
cargo clippy --workspace --all-targets
journalctl --user -f -o cat /usr/bin/gnome-shell   # extension logs
```

## Keyboard shortcuts

| Keys | Action |
|---|---|
| Shift+Super+C | Open or close Laccy |
| Type anything | Search |
| ↑ / ↓ | Move the selection |
| Enter / click | Paste the item (copy only, if `paste_automatically = false`) |
| Alt+Enter | The opposite of Enter: copy only, or paste |
| Ctrl+1 … Ctrl+9 | Pick the first nine items |
| Alt+P | Pin or unpin (pinned items stay at the top) |
| Alt+Delete | Delete the item |
| Ctrl+Shift+Delete | Clear unpinned history |
| Esc | Close |

To change the shortcut:

```sh
gsettings --schemadir ~/.local/share/gnome-shell/extensions/laccy@laccy.github.io/schemas \
  set org.gnome.shell.extensions.laccy toggle-shortcut "['<Super>v']"
```

## Configuration

`~/.config/laccy/config` (optional; changes apply on the next copy or popup):

```ini
# Unpinned items to keep
history_size = 200
# Paste into the focused app after picking an item
paste_automatically = true
# Desktop app IDs whose copies are never recorded
ignored_apps = org.keepassxc.KeePassXC.desktop, com.bitwarden.desktop.desktop
# Larger copies are not recorded
max_item_bytes = 1048576
```

## Known limitations

- **Terminals** paste with Ctrl+Shift+V, so auto-paste doesn't work there yet. Use Alt+Enter and paste yourself.
- **Text only.** Images and files are not recorded yet.
- The popup opens where GNOME places new windows (Wayland apps can't choose their own position).

## Roadmap

- [ ] Preferences window (history size, shortcut, ignored apps, auto-paste)
- [ ] Terminal-aware paste (Ctrl+Shift+V when a terminal is focused)
- [ ] Images and file copies
- [ ] Fuzzy and regex search modes
- [ ] Paste without formatting
- [ ] Live refresh while the popup is open
- [ ] App icon, Flatpak/RPM packaging, extensions.gnome.org release
- [ ] Choose a final app ID and domain (currently `io.github.laccy.Laccy`)

## License

MIT. See [LICENSE](LICENSE).
