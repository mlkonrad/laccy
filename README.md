# Laccy

A lightweight, keyboard-first clipboard manager for GNOME, inspired by
[Maccy](https://github.com/p0deje/Maccy). Press **Shift+Super+C** (or click
the clipboard icon in the top bar), type to search, press **Enter** to copy,
then paste wherever you like.

Laccy is an independent project. It is not affiliated with Maccy and doesn't
share any code with it.

## Status

Early scaffold. It records and pastes text, images and files on GNOME 50 and 51 (Wayland).
See the [roadmap](#roadmap).

## How it works

GNOME doesn't let regular apps watch the clipboard, register global shortcuts
or type keys. Laccy is therefore three small parts:

```
 GNOME Shell extension (JS, runs inside gnome-shell)
   ├─ clipboard changed ──AddText/AddImage/AddFiles──▶ laccyd (Rust, no UI)
   │                                   └─ SQLite ~/.local/share/laccy/history.db
   ├─ top bar menu and label ──Clear/Latest──▶ laccyd
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
thing that stays running (except after you pick [formatted text](#images-files-and-formatted-text)),
and the popup loads only a 10,000-character excerpt
of each item. Images show as small thumbnails, made once when they're copied
by a `laccy --thumbnail` process that exits right away (image decoders hold on
to memory, so they never load in `laccyd`). The popup only decodes the
thumbnails on screen. It opens and closes without GNOME's window animations,
and its text cursor doesn't blink, because the fade redraws the whole popup
every frame.

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
apps or copies matching a pattern, like API keys (see [Preferences](#preferences)).
You can also pause recording, or skip just the next copy, from the
[top bar icon](#top-bar-icon). Nothing is recorded while the
screen is locked, because GNOME disables extensions then. History never
leaves your machine.

## Images, files and formatted text

- **Files** copied in Files (or any app that copies a list of `file://` URIs) are recorded as files. Picking them copies the files again, so you can paste them into a folder or an upload dialog. Search matches their paths.
- **Images** (screenshots, *Copy Image* in a browser) are recorded when the app offers an image and no text. Apps that offer text next to an image, like spreadsheets, are recorded as text. Images have no text to search, so they only show while the search is empty.
- Images larger than 10 MB aren't recorded. Change the limit, in bytes, with the `max-image-bytes` setting (see [Preferences](#preferences)).
- **Formatted text** (bold, links, lists or tables copied from documents, web pages and mail) keeps its HTML and RTF next to the plain text, each up to the `max-item-bytes` limit (1 MB). Picking it pastes the formatting too; **Alt+Shift+Enter** or the **Paste Without Formatting** preference picks the plain text. Apps' own formats, like LibreOffice's, aren't kept, so copies between LibreOffice documents can lose some styling.
- GNOME Shell extensions can only put one format on the clipboard, and GNOME keeps only the plain text when an app that copied something quits. So after you pick formatted text, `laccy` keeps offering it, without a window, until something else is copied, then exits. Meanwhile it uses about 11 MB of private memory (53 MB resident, mostly shared libraries).

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
| Alt+Shift+Enter | Paste the item with the opposite of Paste Without Formatting: as plain text, unless that's on |
| Ctrl+1 … Ctrl+9 | Pick the first nine unpinned items |
| Ctrl+B, Ctrl+D, … | Pick a pinned item by the key shown on its row |
| Alt+P | Pin or unpin (pinned items stay at the top) |
| Alt+Delete | Delete the item |
| Ctrl+Space | Show or hide the preview |
| Ctrl+Shift+Delete | Clear unpinned history |
| Ctrl+Alt+Shift+Delete | Clear all history, including pinned items |
| Ctrl+, | Open preferences |
| Esc | Close |
| Ctrl+Q | Quit (also stops offering formatted text picked earlier) |

Each pinned item gets its own key, like in Maccy: the first free one of
B D E F G H I J K L M N O P R S T U W, in that order. Unpinning frees the key
for the next pin. The letters the search field uses with Ctrl (A, C, V, X, Y,
Z) and Q (quit) aren't used. Past 19 pinned items, new pins get no key.

Like Maccy's menu, the bottom of the popup has **Clear**, **Preferences…**,
**About** and **Quit**. Click one, or press Tab until it's focused, then ↑ / ↓
and Enter. Quit doesn't stop recording, which the extension and `laccyd` do;
use **Pause Recording** for that.

While you search, the matching text is highlighted. The **preview** shows
the selected item in full next to the list: the whole text, the image at
full size, or the paths of copied files, with the app it came from, when it
was first and last copied, and how often. Its pin and trash buttons pin
(or unpin) and delete the item, like Alt+P and Alt+Delete. It opens with
Ctrl+Space, or by itself once the selection rests for 1.5 seconds; after
you close it, it waits for Ctrl+Space until the popup opens again. The
popup widens to the right while it's open.

## Top bar icon

| Click | Action |
|---|---|
| Click | Open or close Laccy |
| Alt+click | Pause or resume recording (the icon dims while paused) |
| Alt+Shift+click | Don't record the next copy |
| Right-click | Menu: Open Clipboard History, Pause Recording, Ignore Next Copy, Clear History…, Preferences |

**Clear History…** asks whether to clear unpinned items or everything.

## Preferences

Open them with **Ctrl+,** in the popup, by right-clicking the clipboard icon
in the top bar, by right-clicking Laccy in the app grid and choosing
**Preferences**, or with `laccy --preferences`.

- **Open Laccy:** the shortcut. Click it and press the new key combination.
- **Paste Automatically:** also paste after picking an item (off by default).
- **Paste Without Formatting:** pick text as plain text, without formatting like bold or links; Alt+Shift+Enter keeps it (off by default).
- **Search Mode:** Exact matches the words you type; Fuzzy also matches their letters in order, so `gco` finds `git checkout`; Regular Expression matches a pattern, ignoring case, so `^https?://` finds items with a line starting with a link; Mixed tries Exact, then Regular Expression, then Fuzzy, and lists the first that finds anything, like Maccy (Exact by default). While a pattern is invalid, the search field turns red, hovering it shows why, and the last results stay listed.
- **Pause Recording:** copies aren't recorded until you turn it off (off by default).

Appearance:

- **Highlight Matches:** Bold, Italic, Underline or Color (Bold by default).
- **Show Special Symbols:** show leading and trailing spaces as `·`, line breaks as `⏎` and tabs as `⇥` (on by default).
- **Show Color Swatches:** items that are only a hex color, like `#3584e4` (`#` and 3, 4, 6 or 8 digits), show a square of that color (on by default).
- **Show App Icons:** the icon of the app each item was copied from (off by default).
- **Show Icon in Top Bar:** see [Top bar icon](#top-bar-icon) (on by default).
- **Show Latest Copy in Top Bar:** the start of the most recently copied unpinned item next to the icon (off by default).

Preview:

- **Open Automatically:** open the preview once the selection rests (on by default).
- **Delay:** how long it has to rest, in milliseconds (1500 by default).

History:

- **History Size:** unpinned items to keep (200 by default).
- **Sort By:** Last Copied, First Copied or Number of Copies (Last Copied by default). Pinned items always stay at the top.
- **Save Text, Save Images, Save Files:** which kinds of copies are recorded (all on by default).
- **Ignored Apps:** apps whose copies are never recorded.
- **Ignored Patterns:** regular expressions; text copies, and file copies by their paths, that match one are never recorded. Patterns are case-sensitive unless they start with `(?i)`, so `(?i)^sk-` skips keys starting with `sk-` or `SK-`.

Changes apply immediately. Settings are stored in GSettings with the
extension's schema, so they also work from the command line:

```sh
gsettings --schemadir ~/.local/share/gnome-shell/extensions/laccy@mlkonrad.github.com/schemas \
  list-recursively org.gnome.shell.extensions.laccy
```

## Known limitations

- **Terminals** paste with Ctrl+Shift+V, so auto-paste (Alt+Enter, or Paste Automatically) doesn't work there yet.
- **Images are kept at full size** in the history database, so a long history of screenshots takes disk space.
- **Picked formatted text** loses its formatting if `laccy` is killed before something else is copied; GNOME then keeps only the plain text.
- The popup opens where GNOME places new windows (Wayland apps can't choose their own position), and the preview widens it to the right.
- **App icons** come from installed apps' desktop files, so copies from apps without one show no icon.
- **Highlights** only mark what's visible in a row, so an item can match further along without anything highlighted; the preview shows the rest.

## Roadmap

- [x] Preferences window (shortcut, auto-paste, history size, ignored apps)
- [ ] Terminal-aware paste (Ctrl+Shift+V when a terminal is focused)
- [x] Images and file copies
- [x] Fuzzy search mode
- [x] Regex search mode
- [x] Mixed search mode
- [x] Formatted text (HTML/RTF) and paste without formatting
- [x] Live refresh while the popup is open
- [x] Pause recording, ignore next copy, ignored patterns, and choosing which kinds of copies are saved
- [x] Sort order, preview, highlighted matches, special symbols, color swatches, app icons
- [x] Clear all history, latest copy in the top bar
- [ ] App icon, Flatpak/RPM packaging, extensions.gnome.org release

## License

MIT. See [LICENSE](LICENSE).
