//! Settings shared by the popup, the daemon and the GNOME Shell extension.
//! They live in GSettings under the extension's schema, so a change made in
//! the preferences window applies everywhere right away.

use std::path::PathBuf;

use gio::prelude::*;

pub const SCHEMA_ID: &str = "org.gnome.shell.extensions.laccy";
pub const EXTENSION_UUID: &str = "laccy@mlkonrad.github.com";

pub const TOGGLE_SHORTCUT_KEY: &str = "laccy-toggle-shortcut";
pub const PASTE_AUTOMATICALLY_KEY: &str = "paste-automatically";
pub const HISTORY_SIZE_KEY: &str = "history-size";
pub const IGNORED_APPS_KEY: &str = "ignored-apps";
pub const MAX_ITEM_BYTES_KEY: &str = "max-item-bytes";
/// Only read by the extension, which adds or removes its top bar icon.
pub const SHOW_INDICATOR_KEY: &str = "show-indicator";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Number of unpinned items kept in history.
    pub history_size: usize,
    /// Also paste into the focused app after picking an item, instead of only
    /// copying it. Alt+Enter does the opposite.
    pub paste_automatically: bool,
    /// Desktop app IDs whose copies are never recorded, e.g. `org.keepassxc.KeePassXC.desktop`.
    pub ignored_apps: Vec<String>,
    /// Copies larger than this are not recorded.
    pub max_item_bytes: usize,
}

/// Used when the schema can't be found. Must match the schema's defaults,
/// which a test checks.
impl Default for Config {
    fn default() -> Self {
        Self {
            history_size: 200,
            paste_automatically: false,
            ignored_apps: Vec::new(),
            max_item_bytes: 1024 * 1024,
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
            ignored_apps: settings.strv(IGNORED_APPS_KEY).iter().map(ToString::to_string).collect(),
            max_item_bytes: settings.uint(MAX_ITEM_BYTES_KEY) as usize,
        }
    }

    pub fn is_ignored(&self, app_id: &str) -> bool {
        !app_id.is_empty() && self.ignored_apps.iter().any(|ignored| ignored == app_id)
    }
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
        settings.set_strv(IGNORED_APPS_KEY, ["org.keepassxc.KeePassXC.desktop"]).unwrap();

        let config = Config::from_settings(&settings);
        assert_eq!(config.history_size, 50);
        assert!(config.paste_automatically);
        assert!(config.is_ignored("org.keepassxc.KeePassXC.desktop"));
        assert!(!config.is_ignored(""));
    }

    #[test]
    fn history_size_range_is_enforced() {
        let settings = test_settings();
        assert!(settings.set_uint(HISTORY_SIZE_KEY, 0).is_err());
    }
}
