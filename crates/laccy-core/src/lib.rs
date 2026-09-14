//! Shared pieces of Laccy: history storage, search, configuration, and the
//! D-Bus names used between the daemon, the popup and the GNOME Shell extension.

pub mod config;
pub mod history;
pub mod search;

use std::path::PathBuf;

/// Application ID of the popup; also the name of its desktop file.
pub const APP_ID: &str = "io.github.laccy.Laccy";

/// Exported by `laccyd`, which the extension calls for every clipboard change.
pub const DAEMON_BUS_NAME: &str = "io.github.laccy.Laccy.Daemon";
pub const DAEMON_OBJECT_PATH: &str = "/io/github/laccy/Laccy/Daemon";
pub const DAEMON_INTERFACE: &str = "io.github.laccy.Daemon1";

/// Exported by the GNOME Shell extension from inside the gnome-shell process.
pub const SHELL_BUS_NAME: &str = "org.gnome.Shell";
pub const SHELL_OBJECT_PATH: &str = "/io/github/laccy/Shell";
pub const SHELL_INTERFACE: &str = "io.github.laccy.Shell1";

pub fn history_path() -> PathBuf {
    xdg_dir("XDG_DATA_HOME", ".local/share").join("laccy/history.db")
}

pub fn config_path() -> PathBuf {
    xdg_dir("XDG_CONFIG_HOME", ".config").join("laccy/config")
}

fn xdg_dir(var: &str, fallback: &str) -> PathBuf {
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
    use super::preview;

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
