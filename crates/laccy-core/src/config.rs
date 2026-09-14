//! Configuration read from `~/.config/laccy/config`, a plain `key = value` file.
//! Missing keys and invalid values fall back to the defaults.

use std::path::Path;

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
    pub fn load() -> Self {
        Self::load_from(&crate::config_path())
    }

    pub fn load_from(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(contents) => Self::parse(&contents),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(err) => {
                eprintln!("laccy: cannot read {}: {err}", path.display());
                Self::default()
            }
        }
    }

    pub fn parse(contents: &str) -> Self {
        let mut config = Self::default();
        for (index, line) in contents.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                eprintln!("laccy: config line {}: expected `key = value`", index + 1);
                continue;
            };
            let (key, value) = (key.trim(), value.trim());
            let valid = match key {
                "history_size" => value
                    .parse()
                    .ok()
                    .filter(|&size| size > 0)
                    .map(|size| config.history_size = size)
                    .is_some(),
                "paste_automatically" => value.parse().map(|v| config.paste_automatically = v).is_ok(),
                "max_item_bytes" => value.parse().map(|v| config.max_item_bytes = v).is_ok(),
                "ignored_apps" => {
                    config.ignored_apps = value
                        .split(',')
                        .map(str::trim)
                        .filter(|app| !app.is_empty())
                        .map(String::from)
                        .collect();
                    true
                }
                _ => {
                    eprintln!("laccy: config line {}: unknown key `{key}`", index + 1);
                    continue;
                }
            };
            if !valid {
                eprintln!("laccy: config line {}: invalid value for `{key}`", index + 1);
            }
        }
        config
    }

    pub fn is_ignored(&self, app_id: &str) -> bool {
        !app_id.is_empty() && self.ignored_apps.iter().any(|ignored| ignored == app_id)
    }
}

#[cfg(test)]
mod tests {
    use super::Config;

    #[test]
    fn empty_file_gives_defaults() {
        assert_eq!(Config::parse("# nothing here\n\n"), Config::default());
    }

    #[test]
    fn parses_all_keys() {
        let config = Config::parse(
            "history_size = 50\npaste_automatically=true\nmax_item_bytes = 10\n\
             ignored_apps = org.keepassxc.KeePassXC.desktop, ,com.bitwarden.desktop.desktop\n",
        );
        assert_eq!(config.history_size, 50);
        assert!(config.paste_automatically);
        assert_eq!(config.max_item_bytes, 10);
        assert!(config.is_ignored("com.bitwarden.desktop.desktop"));
        assert!(!config.is_ignored(""));
    }

    #[test]
    fn invalid_values_keep_defaults() {
        let config = Config::parse("history_size = 0\npaste_automatically = maybe\nbogus\ncolor = red\n");
        assert_eq!(config, Config::default());
    }
}
