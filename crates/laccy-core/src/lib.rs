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

/// The size of the thumbnail of a `width` × `height` image: scaled down,
/// keeping its aspect ratio, to fit the maximum thumbnail size.
pub fn thumbnail_size(width: i32, height: i32) -> (i32, i32) {
    let scale = (f64::from(THUMBNAIL_MAX_WIDTH) / f64::from(width.max(1)))
        .min(f64::from(THUMBNAIL_MAX_HEIGHT) / f64::from(height.max(1)))
        .min(1.0);
    let scaled = |size: i32| ((f64::from(size) * scale).round() as i32).max(1);
    (scaled(width), scaled(height))
}

/// Exported by `laccyd`, which the extension calls for every clipboard change.
pub const DAEMON_BUS_NAME: &str = "io.github.mlkonrad.Laccy.Daemon";
pub const DAEMON_OBJECT_PATH: &str = "/io/github/mlkonrad/Laccy/Daemon";
pub const DAEMON_INTERFACE: &str = "io.github.mlkonrad.Laccy.Daemon1";
/// Emitted by `laccyd` after it stores a copy, so an open popup can reload.
pub const HISTORY_CHANGED_SIGNAL: &str = "HistoryChanged";

/// Exported by the GNOME Shell extension from inside the gnome-shell process.
pub const SHELL_BUS_NAME: &str = "org.gnome.Shell";
pub const SHELL_OBJECT_PATH: &str = "/io/github/mlkonrad/Laccy/Shell";
pub const SHELL_INTERFACE: &str = "io.github.mlkonrad.Laccy.Shell1";

pub fn history_path() -> PathBuf {
    xdg_dir("XDG_DATA_HOME", ".local/share").join("laccy/history.db")
}

pub(crate) fn xdg_dir(var: &str, fallback: &str) -> PathBuf {
    match std::env::var_os(var).map(PathBuf::from) {
        Some(dir) if dir.is_absolute() => dir,
        _ => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(fallback),
    }
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
    use super::{preview, thumbnail_size};

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
}
