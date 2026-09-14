# Laccy

A lightweight, keyboard-first clipboard manager for GNOME, inspired by
[Maccy](https://github.com/p0deje/Maccy). Press **Shift+Super+C** (or click
the clipboard icon in the top bar), type to search, press **Enter** to copy,
then paste wherever you like.

Laccy is an independent project. It is not affiliated with Maccy and doesn't
share any code with it.

## Status

Early scaffold. It records and pastes text, images and files on GNOME 50 (Wayland).
See the [roadmap](#roadmap).

## How it works

GNOME doesn't let regular apps watch the clipboard, register global shortcuts
or type keys. Laccy is therefore three small parts:

```
 GNOME Shell extension (JS, runs inside gnome-shell)
   ├─ clipboard changed ──AddText/AddImage/AddFiles──▶ laccyd (Rust, no UI)
   │                                   └─ SQLite ~/.local/share/laccy/history.db
   ├─ Shift+Super+C ──launches──▶ laccy popup (Rust, GTK 4 + libadwaita)
   │                                   └─ reads the same database, reloads on laccyd's HistoryChanged
   └─ ◀──SetClipboard/SetContent── laccy, when an item is picked
        sets the clipboard, then sends Ctrl+V if asked
```

| Part | Where | Role |
|---|---|---|
| `extension/laccy@mlkonrad.github.com` | inside gnome-shell | Watches the clipboard, handles the shortcut, sets the clipboard and pastes |
| `crates/laccyd` | background, D-Bus activated | Stores copies; only glib and SQLite, no GTK |
| `crates/laccy` | runs only while a window is open | The popup (search, pick, pin, delete) and the preferences window |
| `crates/laccy-core` | library | History database, search, settings, D-Bus names |

**Keeping it lightweight:** no GTK process runs while the popup is hidden.
`laccy` starts on the shortcut and exits when it closes, `laccyd` is the only
thing that stays running, and the popup loads only a 10,000-character excerpt
of each item. Images show as small thumbnails, made once when they're copied
by a `laccy --thumbnail` process that exits right away (image decoders hold on
to memory, so they never load in `laccyd`). The popup only decodes the
thumbnails on screen.

The popup renders with GTK's Cairo renderer instead of the GPU. Measured on
GNOME 50 with 200 items (median of warm launches):

| Renderer | Startup CPU | Resident memory | Private memory |
|---|---|---|---|
| Vulkan (GTK default) | 515 ms | 180 MB | 54 MB |
| OpenGL | 415 ms | 150 MB | 43 MB |
| **Cairo (Laccy default)** | **300 ms** | **60 MB** | **17 MB** |

Set `GSK_RENDERER` to override it. `laccyd` uses about 9 MB resident, 0.7 MB private.
Copied images pass through it, so after large ones it keeps up to about two
images' worth of buffers: 12 MB more private memory after 6 MB screenshots.

**Privacy:** copies marked as secret by password managers
(`x-kde-passwordManagerHint`) are never recorded, and you can ignore whole
apps (see [Preferences](#preferences)). Nothing is recorded while the
screen is locked, because GNOME disables extensions then. History never
leaves your machine.

## Images and files

- **Files** copied in Files (or any app that copies a list of `file://` URIs) are recorded as files. Picking them copies the files again, so you can paste them into a folder or an upload dialog. Search matches their paths.
- **Images** (screenshots, *Copy Image* in a browser) are recorded when the app offers an image and no text. Apps that offer text next to an image, like spreadsheets, are recorded as text. Images have no text to search, so they only show while the search is empty.
- Images larger than 10 MB aren't recorded. Change the limit, in bytes, with the `max-image-bytes` setting (see [Preferences](#preferences)).

## Build and install

Requirements (Fedora): `sudo dnf install cargo rust gtk4-devel libadwaita-devel`

```sh
scripts/install-dev.sh     # builds, installs into ~/.local, links the extension
```

The first time, log out and back in (GNOME Shell only discovers new extensions
at login), then run `gnome-extensions enable laccy@mlkonrad.github.com`.

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
| Enter / click | Copy the item (and paste it, if Paste Automatically is on) |
| Alt+Enter | The opposite of Enter: copy and paste, or copy only |
| Ctrl+1 … Ctrl+9 | Pick the first nine items |
| Alt+P | Pin or unpin (pinned items stay at the top) |
| Alt+Delete | Delete the item |
| Ctrl+Shift+Delete | Clear unpinned history |
| Ctrl+, | Open preferences |
| Esc | Close |

## Preferences

Open them with **Ctrl+,** in the popup, by right-clicking the clipboard icon
in the top bar, by right-clicking Laccy in the app grid and choosing
**Preferences**, or with `laccy --preferences`.

- **Open Laccy:** the shortcut. Click it and press the new key combination.
- **Paste Automatically:** also paste after picking an item (off by default).
- **Search Mode:** Exact matches the words you type; Fuzzy also matches their letters in order, so `gco` finds `git checkout`; Regular Expression matches a pattern, ignoring case, so `^https?://` finds items with a line starting with a link (Exact by default). While a pattern is invalid, the search field turns red, hovering it shows why, and the last results stay listed.
- **Show Icon in Top Bar:** click the icon to open the history, right-click it for a menu (on by default).
- **History Size:** unpinned items to keep (200 by default).
- **Ignored Apps:** apps whose copies are never recorded.

Changes apply immediately. Settings are stored in GSettings with the
extension's schema, so they also work from the command line:

```sh
gsettings --schemadir ~/.local/share/gnome-shell/extensions/laccy@mlkonrad.github.com/schemas \
  list-recursively org.gnome.shell.extensions.laccy
```

## Known limitations

- **Terminals** paste with Ctrl+Shift+V, so auto-paste (Alt+Enter, or Paste Automatically) doesn't work there yet.
- **Images are kept at full size** in the history database, so a long history of screenshots takes disk space.
- The popup opens where GNOME places new windows (Wayland apps can't choose their own position).

## Roadmap

- [x] Preferences window (shortcut, auto-paste, history size, ignored apps)
- [ ] Terminal-aware paste (Ctrl+Shift+V when a terminal is focused)
- [x] Images and file copies
- [x] Fuzzy search mode
- [x] Regex search mode
- [ ] Paste without formatting
- [x] Live refresh while the popup is open
- [ ] App icon, Flatpak/RPM packaging, extensions.gnome.org release

## License

MIT. See [LICENSE](LICENSE).
