//! Shared pieces of Laccy: history storage, search, configuration, and the
//! D-Bus names used between the daemon, the popup and the GNOME Shell extension.

pub mod config;
pub mod files;
pub mod history;
pub mod search;

use std::path::PathBuf;

/// Application ID of the popup; also the name of its desktop file.
pub const APP_ID: &str = "io.github.mlkonrad.Laccy";

/// Largest image thumbnail, in pixels. The popup shows thumbnails at half
/// this size, so they stay sharp on screens scaled to 200%.
pub const THUMBNAIL_MAX_WIDTH: i32 = 640;
pub const THUMBNAIL_MAX_HEIGHT: i32 = 96;
/// How many thumbnail pixels the popup draws per logical pixel.
pub const THUMBNAIL_SCALE: i32 = 2;

/// The size of the thumbnail of a `width` x `height` image: scaled down,
/// keeping its aspect ratio, to fit the maximum thumbnail size.
pub fn thumbnail_size(width: i32, height: i32) -> (i32, i32) {
    let scale = (f64::from(THUMBNAIL_MAX_WIDTH) / f64::from(width.max(1)))
        .min(f64::from(THUMBNAIL_MAX_HEIGHT) / f64::from(height.max(1)))
        .min(1.0);
    let scaled = |size: i32| ((f64::from(size) * scale).round() as i32).max(1);
    (scaled(width), scaled(height))
}

/// The image formats recorded, most preferred first. Anything else an app
/// copies would reach a rarely used image loader, some of which (like XPM's)
/// still run outside glycin's sandbox. The extension has its own copy of this list.
pub const IMAGE_MIME_TYPES: [&str; 6] = ["image/png", "image/jpeg", "image/gif", "image/webp", "image/bmp", "image/tiff"];

/// Whether `data` starts like an image in `mime_type`, one of
/// [`IMAGE_MIME_TYPES`]. Image loaders (glycin's too) pick a decoder from the
/// data, not from the type it was copied as, so the type alone keeps nothing out.
pub fn is_image_of_type(mime_type: &str, data: &[u8]) -> bool {
    match mime_type {
        "image/png" => data.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => data.starts_with(&[0xff, 0xd8, 0xff]),
        "image/gif" => data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a"),
        "image/webp" => data.len() >= 12 && data.starts_with(b"RIFF") && &data[8..12] == b"WEBP",
        "image/bmp" => data.starts_with(b"BM"),
        "image/tiff" => data.starts_with(b"II*\0") || data.starts_with(b"MM\0*"),
        _ => false,
    }
}

/// Images with more pixels than this are not recorded. A small file can
/// declare a huge image that takes gigabytes to decode; this is about
/// 100 megapixels, more than three 8K screens.
pub const MAX_IMAGE_PIXELS: i64 = 100_000_000;

/// Exported by `laccyd`, which the extension calls for every clipboard change.
pub const DAEMON_BUS_NAME: &str = "io.github.mlkonrad.Laccy.Daemon";
pub const DAEMON_OBJECT_PATH: &str = "/io/github/mlkonrad/Laccy/Daemon";
pub const DAEMON_INTERFACE: &str = "io.github.mlkonrad.Laccy.Daemon1";
/// Emitted by `laccyd` after it stores a copy, so an open popup can reload.
pub const HISTORY_CHANGED_SIGNAL: &str = "HistoryChanged";

/// Formatted versions of copied text kept next to the plain text. The
/// extension has its own copy of this list.
pub const FORMAT_MIME_TYPES: [&str; 3] = ["text/html", "text/rtf", "application/rtf"];
/// Offered by `laccy` next to a formatted item it puts back on the clipboard,
/// so the extension doesn't record that copy again.
pub const PICKED_MIME_TYPE: &str = "application/x-laccy-picked";

/// Exported by the GNOME Shell extension from inside the gnome-shell process.
pub const SHELL_BUS_NAME: &str = "org.gnome.Shell";
pub const SHELL_OBJECT_PATH: &str = "/org/gnome/Shell/Extensions/Laccy";
pub const SHELL_INTERFACE: &str = "org.gnome.Shell.Extensions.Laccy";

/// Marks that this login has started. `$XDG_RUNTIME_DIR` is emptied when the
/// user logs out and when the computer shuts down, even without a clean
/// shutdown, so a missing marker means a new login.
fn login_marker_path() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?);
    dir.is_absolute().then(|| dir.join("laccy-login"))
}

/// Run when `laccyd` or the popup starts. The first of them in a login marks
/// it, and with `clear_on_logout` removes the unpinned items left from the
/// last login. Laccy can't do it at logout itself: nothing of it reliably
/// runs then, and laccyd is also stopped when it's reinstalled. Returns whether it cleared.
pub fn clear_if_new_login(history: &history::History, clear_on_logout: bool) -> bool {
    let Some(marker) = login_marker_path() else {
        return false;
    };
    match clear_if_new_login_at(&marker, history, clear_on_logout) {
        Ok(cleared) => cleared,
        Err(err) => {
            eprintln!("laccy: cannot check for a new login: {err}");
            false
        }
    }
}

fn clear_if_new_login_at(
    marker: &std::path::Path,
    history: &history::History,
    clear_on_logout: bool,
) -> Result<bool, Box<dyn std::error::Error>> {
    // Created whatever the setting, so turning it on mid-login doesn't clear
    // right away. Only one process can create it, so only one clears.
    match std::fs::OpenOptions::new().write(true).create_new(true).open(marker) {
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(err) => return Err(err.into()),
    }
    if !clear_on_logout {
        return Ok(false);
    }
    history.clear(false)?;
    Ok(true)
}

pub fn history_path() -> PathBuf {
    xdg_dir("XDG_DATA_HOME", ".local/share").join("laccy/history.db")
}

pub(crate) fn xdg_dir(var: &str, fallback: &str) -> PathBuf {
    match std::env::var_os(var).map(PathBuf::from) {
        Some(dir) if dir.is_absolute() => dir,
        _ => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(fallback),
    }
}

/// A single line of `text` for a popup row, cut to `max_chars` characters.
/// Without `special_symbols` it's the [`preview`]. With them, whitespace shows
/// as symbols: leading and trailing spaces as a middle dot (U+00B7), line
/// breaks as a return symbol (U+23CE) and tabs as a tab arrow (U+21E5).
pub fn display_text(text: &str, max_chars: usize, special_symbols: bool) -> String {
    if !special_symbols {
        return preview(text, max_chars);
    }
    let body = text.trim_matches(' ');
    let leading = text.len() - text.trim_start_matches(' ').len();
    // Text of only spaces is all leading.
    let trailing = if body.is_empty() { 0 } else { text.len() - text.trim_end_matches(' ').len() };
    let body = body.replace("\r\n", "\n");
    let mut chars = std::iter::repeat_n('·', leading)
        .chain(body.chars().map(|c| match c {
            '\n' | '\r' => '⏎',
            '\t' => '⇥',
            c => c,
        }))
        .chain(std::iter::repeat_n('·', trailing));
    let mut out: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        out.push('…');
    }
    out
}

/// The color of an item that is only a hex color code, like `#3584e4`, as red,
/// green, blue and alpha. Takes `#` and 3, 4, 6 or 8 hex digits. The `#`
/// is required, so words like "decade" don't count.
pub fn hex_color(text: &str) -> Option<[u8; 4]> {
    let digits = text.trim().strip_prefix('#')?;
    if !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    // All ASCII now, so every byte index is a character boundary.
    let hex = |start: usize, len: usize| u8::from_str_radix(&digits[start..start + len], 16).ok();
    let channels: Vec<u8> = match digits.len() {
        3 | 4 => (0..digits.len()).map(|i| hex(i, 1).map(|nibble| nibble * 17)).collect::<Option<_>>()?,
        6 | 8 => (0..digits.len()).step_by(2).map(|i| hex(i, 2)).collect::<Option<_>>()?,
        _ => return None,
    };
    Some([channels[0], channels[1], channels[2], channels.get(3).copied().unwrap_or(255)])
}

/// Characters of the latest copy shown next to the top bar icon.
pub const LATEST_COPY_CHARS: usize = 20;

/// The label of `item` next to the top bar icon: the start of its text, the
/// names of its files, or "Image".
pub fn latest_copy_label(item: &history::Item) -> String {
    let text = match item.kind {
        history::Kind::Text => item.excerpt.chars().take(100).collect(),
        history::Kind::Files => item.excerpt.lines().map(files::display_name).collect::<Vec<_>>().join(", "),
        history::Kind::Image => "Image".to_owned(),
    };
    preview(&text, LATEST_COPY_CHARS)
}

/// Single-line preview of `text`: whitespace runs collapse to one space and the
/// result is cut to `max_chars` characters, ending in an ellipsis when cut.
pub fn preview(text: &str, max_chars: usize) -> String {
    let mut out = String::new();
    let mut len = 0;
    for word in text.split_whitespace() {
        let separator = (len > 0).then_some(' ');
        for c in separator.into_iter().chain(word.chars()) {
            if len == max_chars {
                out.push('…');
                return out;
            }
            out.push(c);
            len += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::history::{Content, History, Item, Kind, PinPosition, SortOrder};
    use super::clear_if_new_login_at;
    use super::{display_text, hex_color, latest_copy_label, preview, thumbnail_size};

    #[test]
    fn latest_copy_labels_are_short() {
        let item = |kind, excerpt: &str| Item {
            id: 1,
            kind,
            excerpt: excerpt.to_owned(),
            source_app: None,
            pinned: false,
            pin_key: None,
            name: None,
            first_copied: 0,
            last_copied: 0,
            copy_count: 1,
        };
        assert_eq!(latest_copy_label(&item(Kind::Text, "  git\n  checkout main  ")), "git checkout main");
        assert_eq!(latest_copy_label(&item(Kind::Text, "a very long copied sentence")), "a very long copied s…");
        assert_eq!(latest_copy_label(&item(Kind::Files, "file:///a/report.pdf\nfile:///b/c.png")), "report.pdf, c.png");
        assert_eq!(latest_copy_label(&item(Kind::Image, "")), "Image");
    }

    #[test]
    fn special_symbols_show_whitespace() {
        assert_eq!(display_text("  a b\n\tc\r\nd  ", 80, true), "··a b⏎⇥c⏎d··");
        assert_eq!(display_text("   ", 80, true), "···");
        assert_eq!(display_text("a\nb", 80, false), "a b");
        assert_eq!(display_text("ab\ncd", 3, true), "ab⏎…");
        assert_eq!(display_text("abc", 3, true), "abc");
    }

    #[test]
    fn hex_colors_need_a_hash_and_hex_digits() {
        assert_eq!(hex_color("#3584e4"), Some([0x35, 0x84, 0xe4, 255]));
        assert_eq!(hex_color(" #FFF\n"), Some([255, 255, 255, 255]));
        assert_eq!(hex_color("#f008"), Some([255, 0, 0, 0x88]));
        assert_eq!(hex_color("#11223344"), Some([0x11, 0x22, 0x33, 0x44]));
        assert_eq!(hex_color("decade"), None);
        assert_eq!(hex_color("#12345"), None);
        assert_eq!(hex_color("#ggg"), None);
        assert_eq!(hex_color("#fff is white"), None);
        assert_eq!(hex_color("#éé"), None);
    }

    #[test]
    fn images_must_start_like_their_type() {
        use super::{IMAGE_MIME_TYPES, is_image_of_type};
        assert!(is_image_of_type("image/png", b"\x89PNG\r\n\x1a\nrest"));
        assert!(is_image_of_type("image/jpeg", b"\xff\xd8\xff\xe0"));
        assert!(is_image_of_type("image/webp", b"RIFF\0\0\0\0WEBPVP8 "));
        assert!(!is_image_of_type("image/webp", b"RIFF\0\0\0\0WAVE"));
        assert!(!is_image_of_type("image/png", b"/* XPM */"));
        assert!(!is_image_of_type("image/x-xpixmap", b"/* XPM */"));
        for mime_type in IMAGE_MIME_TYPES {
            assert!(!is_image_of_type(mime_type, b""), "{mime_type}");
        }
    }

    #[test]
    fn thumbnails_fit_and_keep_aspect_ratio() {
        assert_eq!(thumbnail_size(1920, 1080), (171, 96));
        assert_eq!(thumbnail_size(6000, 100), (640, 11));
        assert_eq!(thumbnail_size(40, 30), (40, 30));
        assert_eq!(thumbnail_size(10_000, 1), (640, 1));
    }

    #[test]
    fn preview_collapses_whitespace() {
        assert_eq!(preview("  fn main() {\n\tprintln!();\n}  ", 80), "fn main() { println!(); }");
    }

    #[test]
    fn preview_truncates_with_ellipsis() {
        assert_eq!(preview("hello world", 5), "hello…");
        assert_eq!(preview("hello", 5), "hello");
        assert_eq!(preview("héllo wörld", 7), "héllo w…");
    }

    #[test]
    fn only_a_new_login_clears_and_only_when_asked() {
        let dir = std::env::temp_dir().join(format!("laccy-test-login-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let marker = dir.join("laccy-login");
        let mut history = History::open_in_memory().unwrap();
        let texts = |history: &History| -> Vec<String> {
            history.items(SortOrder::LastCopied, PinPosition::Top).unwrap().into_iter().map(|item| item.excerpt).collect()
        };
        let pinned = history.add(&Content::Text("pinned".into()), None, 10).unwrap();
        history.set_pinned(pinned, true).unwrap();
        history.add(&Content::Text("copied".into()), None, 10).unwrap();

        // Off: the login is marked, nothing is cleared, and turning it on later
        // in the same login doesn't clear either.
        assert!(!clear_if_new_login_at(&marker, &history, false).unwrap());
        assert!(!clear_if_new_login_at(&marker, &history, true).unwrap());
        assert_eq!(texts(&history), ["pinned", "copied"]);

        // The next login (the marker is gone) keeps only pinned items.
        std::fs::remove_file(&marker).unwrap();
        assert!(clear_if_new_login_at(&marker, &history, true).unwrap());
        assert_eq!(texts(&history), ["pinned"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn cleared_items_leave_no_trace_in_the_file() {
        let dir = std::env::temp_dir().join(format!("laccy-test-secure-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.db");
        {
            let mut history = History::open(&path).unwrap();
            history.add(&Content::Text("kept".into()), None, 10).unwrap();
            history.add(&Content::Text("secret-4f9d2c".into()), None, 10).unwrap();
            let kept = history.items(SortOrder::LastCopied, PinPosition::Top).unwrap()[1].id;
            history.set_pinned(kept, true).unwrap();
            history.clear(false).unwrap();
            // Still open, as laccyd keeps it: the file and its log must already be clean.
            for entry in std::fs::read_dir(&dir).unwrap() {
                let bytes = std::fs::read(entry.unwrap().path()).unwrap();
                assert!(!bytes.windows(13).any(|w| w == b"secret-4f9d2c"));
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
