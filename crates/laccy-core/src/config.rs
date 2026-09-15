//! Settings shared by the popup, the daemon and the GNOME Shell extension.
//! They live in GSettings under the extension's schema, so a change made in
//! the preferences window applies everywhere right away.

use std::path::PathBuf;

use gio::glib;
use gio::prelude::*;

use crate::history::{Content, Format, SortOrder};
use crate::{FORMAT_MIME_TYPES, files, search};

pub const SCHEMA_ID: &str = "org.gnome.shell.extensions.laccy";
pub const EXTENSION_UUID: &str = "laccy@mlkonrad.github.com";

pub const TOGGLE_SHORTCUT_KEY: &str = "laccy-toggle-shortcut";
pub const PASTE_AUTOMATICALLY_KEY: &str = "paste-automatically";
pub const PASTE_WITHOUT_FORMATTING_KEY: &str = "paste-without-formatting";
pub const HISTORY_SIZE_KEY: &str = "history-size";
pub const IGNORED_APPS_KEY: &str = "ignored-apps";
pub const IGNORED_PATTERNS_KEY: &str = "ignored-patterns";
/// Only read by the extension, which stops passing copies on while it's set.
pub const PAUSED_KEY: &str = "paused";
/// Also read by the extension, so it doesn't read content the daemon would drop.
pub const RECORD_TEXT_KEY: &str = "record-text";
pub const RECORD_IMAGES_KEY: &str = "record-images";
pub const RECORD_FILES_KEY: &str = "record-files";
pub const MAX_ITEM_BYTES_KEY: &str = "max-item-bytes";
/// Also read by the extension, so it doesn't send the daemon images it would drop.
pub const MAX_IMAGE_BYTES_KEY: &str = "max-image-bytes";
pub const SEARCH_MODE_KEY: &str = "search-mode";
pub const SORT_BY_KEY: &str = "sort-by";
pub const HIGHLIGHT_MATCHES_KEY: &str = "highlight-matches";
pub const SHOW_SPECIAL_SYMBOLS_KEY: &str = "show-special-symbols";
pub const SHOW_COLOR_SWATCHES_KEY: &str = "show-color-swatches";
pub const SHOW_APP_ICONS_KEY: &str = "show-app-icons";
/// Only read by the extension, which adds or removes its top bar icon.
pub const SHOW_INDICATOR_KEY: &str = "show-indicator";

/// How the popup marks the text a search matched.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Highlight {
    #[default]
    Bold,
    Italic,
    Underline,
    Color,
}

impl Highlight {
    /// Every style, in the order the preferences window lists them.
    pub const ALL: [Self; 4] = [Self::Bold, Self::Italic, Self::Underline, Self::Color];

    /// The style's value in GSettings.
    pub fn name(self) -> &'static str {
        match self {
            Self::Bold => "bold",
            Self::Italic => "italic",
            Self::Underline => "underline",
            Self::Color => "color",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|style| style.name() == name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Number of unpinned items kept in history.
    pub history_size: usize,
    /// Also paste into the focused app after picking an item, instead of only
    /// copying it. Alt+Enter does the opposite.
    pub paste_automatically: bool,
    /// Pick text items as plain text, without their formatting.
    /// Alt+Shift+Enter does the opposite.
    pub paste_without_formatting: bool,
    /// Desktop app IDs whose copies are never recorded, e.g. `org.keepassxc.KeePassXC.desktop`.
    pub ignored_apps: Vec<String>,
    /// Regular expressions; text and file copies matching any of them are not recorded.
    pub ignored_patterns: Vec<String>,
    /// Which kinds of copies are recorded.
    pub record_text: bool,
    pub record_images: bool,
    pub record_files: bool,
    /// Text and file copies larger than this are not recorded.
    pub max_item_bytes: usize,
    /// Image copies larger than this are not recorded.
    pub max_image_bytes: usize,
    /// How the popup matches the search query.
    pub search_mode: search::Mode,
    /// The order of unpinned items.
    pub sort_by: SortOrder,
    /// How the popup marks the text a search matched.
    pub highlight_matches: Highlight,
    /// Show whitespace as symbols in the popup, see [`crate::display_text`].
    pub show_special_symbols: bool,
    /// Show a swatch next to items that are a hex color, see [`crate::hex_color`].
    pub show_color_swatches: bool,
    /// Show the icon of the app each item was copied from.
    pub show_app_icons: bool,
}

/// Used when the schema can't be found. Must match the schema's defaults,
/// which a test checks.
impl Default for Config {
    fn default() -> Self {
        Self {
            history_size: 200,
            paste_automatically: false,
            paste_without_formatting: false,
            ignored_apps: Vec::new(),
            ignored_patterns: Vec::new(),
            record_text: true,
            record_images: true,
            record_files: true,
            max_item_bytes: 1024 * 1024,
            max_image_bytes: 10 * 1024 * 1024,
            search_mode: search::Mode::Exact,
            sort_by: SortOrder::LastCopied,
            highlight_matches: Highlight::Bold,
            show_special_symbols: true,
            show_color_swatches: true,
            show_app_icons: false,
        }
    }
}

impl Config {
    /// The current settings, or the defaults if the extension isn't installed.
    pub fn load() -> Self {
        settings().map_or_else(Self::default, |settings| Self::from_settings(&settings))
    }

    pub fn from_settings(settings: &gio::Settings) -> Self {
        Self {
            history_size: settings.uint(HISTORY_SIZE_KEY) as usize,
            paste_automatically: settings.boolean(PASTE_AUTOMATICALLY_KEY),
            paste_without_formatting: settings.boolean(PASTE_WITHOUT_FORMATTING_KEY),
            ignored_apps: settings.strv(IGNORED_APPS_KEY).iter().map(ToString::to_string).collect(),
            ignored_patterns: settings.strv(IGNORED_PATTERNS_KEY).iter().map(ToString::to_string).collect(),
            record_text: settings.boolean(RECORD_TEXT_KEY),
            record_images: settings.boolean(RECORD_IMAGES_KEY),
            record_files: settings.boolean(RECORD_FILES_KEY),
            max_item_bytes: settings.uint(MAX_ITEM_BYTES_KEY) as usize,
            max_image_bytes: settings.uint(MAX_IMAGE_BYTES_KEY) as usize,
            search_mode: search::Mode::from_name(&settings.string(SEARCH_MODE_KEY)).unwrap_or_default(),
            sort_by: SortOrder::from_name(&settings.string(SORT_BY_KEY)).unwrap_or_default(),
            highlight_matches: Highlight::from_name(&settings.string(HIGHLIGHT_MATCHES_KEY)).unwrap_or_default(),
            show_special_symbols: settings.boolean(SHOW_SPECIAL_SYMBOLS_KEY),
            show_color_swatches: settings.boolean(SHOW_COLOR_SWATCHES_KEY),
            show_app_icons: settings.boolean(SHOW_APP_ICONS_KEY),
        }
    }

    pub fn is_ignored(&self, app_id: &str) -> bool {
        !app_id.is_empty() && self.ignored_apps.iter().any(|ignored| ignored == app_id)
    }

    /// Whether a copy of `content` from `source_app` should be recorded.
    pub fn should_record(&self, content: &Content, source_app: &str) -> bool {
        if self.is_ignored(source_app) {
            return false;
        }
        match content {
            Content::Text(text) => {
                self.record_text
                    && !text.trim().is_empty()
                    && text.len() <= self.max_item_bytes
                    && !self.matches_ignored_pattern(text)
            }
            Content::Files(uris) => {
                self.record_files
                    && !uris.is_empty()
                    && uris.iter().map(String::len).sum::<usize>() <= self.max_item_bytes
                    && uris.iter().all(|uri| files::path(uri).is_some())
                    && !self.matches_ignored_pattern(
                        &uris.iter().map(|uri| files::display_path(uri)).collect::<Vec<_>>().join("\n"),
                    )
            }
            Content::Image { mime_type, data } => {
                self.record_images
                    && mime_type.starts_with("image/")
                    && !data.is_empty()
                    && data.len() <= self.max_image_bytes
            }
        }
    }

    /// Whether any ignored pattern matches somewhere in `text`. Invalid patterns
    /// match nothing, as in Maccy.
    fn matches_ignored_pattern(&self, text: &str) -> bool {
        use glib::IntoGStr;

        self.ignored_patterns.iter().any(|pattern| {
            compile_pattern(pattern).ok().flatten().is_some_and(|regex| {
                text.run_with_gstr(|text| {
                    regex.match_(text, glib::RegexMatchFlags::DEFAULT).is_ok_and(|info| info.matches())
                })
            })
        })
    }

    /// The formatted versions of a text copy worth keeping: known formats no
    /// larger than a text copy may be.
    pub fn formats_to_record(&self, formats: Vec<Format>) -> Vec<Format> {
        formats
            .into_iter()
            .filter(|format| {
                FORMAT_MIME_TYPES.contains(&format.mime_type.as_str())
                    && !format.data.is_empty()
                    && format.data.len() <= self.max_item_bytes
            })
            .collect()
    }
}

/// Why `pattern` can't be an ignored pattern, or `None` if it's a valid one.
pub fn pattern_error(pattern: &str) -> Option<String> {
    if pattern.is_empty() {
        return Some("The pattern is empty".to_owned());
    }
    compile_pattern(pattern).err().map(|err| err.message().to_owned())
}

/// Case-sensitive like Maccy's patterns; `(?i)` turns that off.
fn compile_pattern(pattern: &str) -> Result<Option<glib::Regex>, glib::Error> {
    glib::Regex::new(pattern, glib::RegexCompileFlags::DEFAULT, glib::RegexMatchFlags::DEFAULT)
}

/// Laccy's settings, using the schema that ships with the GNOME Shell
/// extension. `None` if the extension isn't installed.
pub fn settings() -> Option<gio::Settings> {
    let default_source = gio::SettingsSchemaSource::default();
    let schema = default_source
        .as_ref()
        .and_then(|source| source.lookup(SCHEMA_ID, true))
        .or_else(|| {
            extension_schema_dirs().into_iter().find_map(|dir| {
                gio::SettingsSchemaSource::from_directory(&dir, default_source.as_ref(), false)
                    .ok()?
                    .lookup(SCHEMA_ID, false)
            })
        })?;
    Some(gio::Settings::new_full(&schema, None::<&gio::SettingsBackend>, None))
}

/// Where GNOME Shell looks for the extension: the user's data dir first, then
/// the system ones.
fn extension_schema_dirs() -> Vec<PathBuf> {
    let data_dirs = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|dirs| !dirs.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    std::iter::once(crate::xdg_dir("XDG_DATA_HOME", ".local/share"))
        .chain(data_dirs.split(':').map(PathBuf::from))
        .map(|dir| dir.join("gnome-shell/extensions").join(EXTENSION_UUID).join("schemas"))
        .filter(|dir| dir.join("gschemas.compiled").is_file())
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// Compiles the extension's schema into a temporary directory and opens it
    /// with an in-memory backend, so tests never touch real settings.
    fn test_settings() -> gio::Settings {
        static RUN: AtomicUsize = AtomicUsize::new(0);
        let schemas = concat!(env!("CARGO_MANIFEST_DIR"), "/../../extension/laccy@mlkonrad.github.com/schemas");
        let dir = std::env::temp_dir().join(format!(
            "laccy-schema-test-{}-{}",
            std::process::id(),
            RUN.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let status = std::process::Command::new("glib-compile-schemas")
            .arg("--strict")
            .arg("--targetdir")
            .arg(&dir)
            .arg(schemas)
            .status()
            .expect("glib-compile-schemas is installed");
        assert!(status.success(), "schema failed to compile");

        let source = gio::SettingsSchemaSource::from_directory(&dir, None, false).unwrap();
        let schema = source.lookup(SCHEMA_ID, false).expect("schema is in the compiled file");
        std::fs::remove_dir_all(&dir).unwrap();
        gio::Settings::new_full(&schema, Some(&gio::memory_settings_backend_new()), None)
    }

    #[test]
    fn schema_defaults_match_config_defaults() {
        assert_eq!(Config::from_settings(&test_settings()), Config::default());
    }

    #[test]
    fn reads_changed_settings() {
        let settings = test_settings();
        settings.set_uint(HISTORY_SIZE_KEY, 50).unwrap();
        settings.set_boolean(PASTE_AUTOMATICALLY_KEY, true).unwrap();
        settings.set_boolean(PASTE_WITHOUT_FORMATTING_KEY, true).unwrap();
        settings.set_strv(IGNORED_APPS_KEY, ["org.keepassxc.KeePassXC.desktop"]).unwrap();
        settings.set_string(SEARCH_MODE_KEY, "fuzzy").unwrap();
        settings.set_strv(IGNORED_PATTERNS_KEY, ["^sk-"]).unwrap();
        settings.set_boolean(RECORD_IMAGES_KEY, false).unwrap();

        let config = Config::from_settings(&settings);
        assert_eq!(config.ignored_patterns, ["^sk-"]);
        assert!(config.record_text && !config.record_images && config.record_files);
        assert_eq!(config.history_size, 50);
        assert!(config.paste_automatically);
        assert!(config.paste_without_formatting);
        assert_eq!(config.search_mode, search::Mode::Fuzzy);
        assert!(config.is_ignored("org.keepassxc.KeePassXC.desktop"));
        assert!(!config.is_ignored(""));
    }

    #[test]
    fn records_only_acceptable_copies() {
        let config = Config {
            max_item_bytes: 20,
            max_image_bytes: 4,
            ignored_apps: vec!["secret.desktop".to_owned()],
            ..Config::default()
        };
        let text = |text: &str| Content::Text(text.to_owned());
        let files = |uris: &[&str]| Content::Files(uris.iter().map(|uri| uri.to_string()).collect());
        let image = |mime_type: &str, data: &[u8]| Content::Image { mime_type: mime_type.to_owned(), data: data.to_vec() };

        assert!(config.should_record(&text("hello"), ""));
        assert!(!config.should_record(&text("hello"), "secret.desktop"));
        assert!(!config.should_record(&text(" \n\t"), ""));
        assert!(!config.should_record(&text(&"x".repeat(21)), ""));

        assert!(config.should_record(&files(&["file:///a", "file:///b"]), ""));
        assert!(!config.should_record(&files(&[]), ""));
        assert!(!config.should_record(&files(&["file:///a", "https://example.com/"]), ""));
        assert!(!config.should_record(&files(&["file:///aaaaaaaaaa", "file:///bbbbbbbbbb"]), ""));

        assert!(config.should_record(&image("image/png", b"1234"), ""));
        assert!(!config.should_record(&image("image/png", b""), ""));
        assert!(!config.should_record(&image("image/png", b"12345"), ""));
        assert!(!config.should_record(&image("text/html", b"1234"), ""));
    }

    #[test]
    fn records_only_enabled_kinds() {
        let text = Content::Text("hello".to_owned());
        let files = Content::Files(vec!["file:///a".to_owned()]);
        let image = Content::Image { mime_type: "image/png".to_owned(), data: b"1234".to_vec() };

        let config = Config { record_text: false, ..Config::default() };
        assert!(!config.should_record(&text, ""));
        assert!(config.should_record(&files, "") && config.should_record(&image, ""));

        let config = Config { record_files: false, ..Config::default() };
        assert!(!config.should_record(&files, ""));
        assert!(config.should_record(&text, "") && config.should_record(&image, ""));

        let config = Config { record_images: false, ..Config::default() };
        assert!(!config.should_record(&image, ""));
        assert!(config.should_record(&text, "") && config.should_record(&files, ""));
    }

    #[test]
    fn ignored_patterns_skip_matching_copies() {
        let config = Config {
            ignored_patterns: vec!["^sk-[a-z0-9]+$".to_owned(), "(?i)secret".to_owned(), "(unclosed".to_owned()],
            ..Config::default()
        };
        let text = |text: &str| Content::Text(text.to_owned());

        assert!(!config.should_record(&text("sk-abc123"), ""));
        // `^` and `$` match at the ends of the whole text only.
        assert!(config.should_record(&text("key:\nsk-abc123\n"), ""));
        assert!(!config.should_record(&text("my SECRET note"), ""));
        // Patterns are case-sensitive without (?i); the invalid one matches nothing.
        assert!(config.should_record(&text("SK-ABC123"), ""));
        assert!(config.should_record(&text("(unclosed"), ""));
        // Files match by their paths.
        assert!(!config.should_record(&Content::Files(vec!["file:///home/me/Secret%20Plans.txt".to_owned()]), ""));
    }

    #[test]
    fn validates_patterns() {
        assert_eq!(pattern_error("^sk-"), None);
        assert!(pattern_error("(unclosed").is_some());
        assert!(pattern_error("").is_some());
    }

    #[test]
    fn records_only_known_small_formats() {
        let config = Config { max_item_bytes: 4, ..Config::default() };
        let format = |mime_type: &str, data: &[u8]| Format { mime_type: mime_type.to_owned(), data: data.to_vec() };
        let formats = vec![
            format("text/html", b"<b>"),
            format("text/rtf", b"{\\rtf1 too long}"),
            format("application/x-private", b"x"),
            format("application/rtf", b""),
        ];
        assert_eq!(config.formats_to_record(formats), [format("text/html", b"<b>")]);
    }

    #[test]
    fn invalid_values_are_rejected() {
        let settings = test_settings();
        assert!(settings.set_uint(HISTORY_SIZE_KEY, 0).is_err());
        assert!(settings.set_string(SEARCH_MODE_KEY, "sloppy").is_err());
    }

    #[test]
    fn schema_lists_every_search_mode() {
        let settings = test_settings();
        for mode in search::Mode::ALL {
            settings.set_string(SEARCH_MODE_KEY, mode.name()).unwrap();
            assert_eq!(Config::from_settings(&settings).search_mode, mode);
        }
    }

    #[test]
    fn schema_lists_every_highlight_style() {
        let settings = test_settings();
        for style in Highlight::ALL {
            settings.set_string(HIGHLIGHT_MATCHES_KEY, style.name()).unwrap();
            assert_eq!(Config::from_settings(&settings).highlight_matches, style);
        }
    }

    #[test]
    fn schema_lists_every_sort_order() {
        let settings = test_settings();
        for order in SortOrder::ALL {
            settings.set_string(SORT_BY_KEY, order.name()).unwrap();
            assert_eq!(Config::from_settings(&settings).sort_by, order);
        }
    }
}
