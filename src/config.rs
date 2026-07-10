//! User configuration loading.
//!
//! The config file is `config.toml`, resolved in this order (first hit wins):
//! 1. `$SSH_FILES_CONFIG` — explicit path, used unconditionally when set
//! 2. `$XDG_CONFIG_HOME/ssh-files/config.toml`, falling back to
//!    `~/.config/ssh-files/config.toml` — checked on every platform, since
//!    that is where command-line users keep dotfiles even on macOS
//! 3. The platform-native config dir via `directories`
//!    (`%APPDATA%\ssh-files\config\` on Windows,
//!    `~/Library/Application Support/ssh-files/` on macOS)
//!
//! Errors never abort startup: they are returned as warnings and the
//! remaining valid configuration is used.

use std::path::PathBuf;

use crate::keymap::Keymap;
use crate::theme::ThemeOverrides;
use crate::ui::icons::{self, IconSet};

pub struct Config {
    pub keymap: Keymap,
    pub theme: ThemeOverrides,
    /// Explicit icon set from `[ui] icons`; None means auto-detect.
    pub icons: Option<&'static IconSet>,
    pub warnings: Vec<String>,
}

pub fn load() -> Config {
    let mut keymap = Keymap::default();
    let mut theme = ThemeOverrides::default();
    let mut icons_choice = None;
    let mut warnings = Vec::new();

    if let Some(path) = find_config_file() {
        match std::fs::read_to_string(&path) {
            Ok(content) => match content.parse::<toml::Table>() {
                Ok(table) => {
                    if let Some(keys) = table.get("keys") {
                        match keys.as_table() {
                            Some(keys) => {
                                for warning in keymap.apply(keys) {
                                    warnings.push(format!("{}: [keys] {}", path.display(), warning));
                                }
                            }
                            None => {
                                warnings.push(format!("{}: [keys] must be a table", path.display()));
                            }
                        }
                    }
                    if let Some(colors) = table.get("theme") {
                        match colors.as_table() {
                            Some(colors) => {
                                let (overrides, theme_warnings) = ThemeOverrides::from_table(colors);
                                theme = overrides;
                                for warning in theme_warnings {
                                    warnings.push(format!("{}: [theme] {}", path.display(), warning));
                                }
                            }
                            None => {
                                warnings.push(format!("{}: [theme] must be a table", path.display()));
                            }
                        }
                    }
                    if let Some(ui) = table.get("ui") {
                        match ui.as_table() {
                            Some(ui) => {
                                if let Some(value) = ui.get("icons") {
                                    match value.as_str() {
                                        Some("auto") => {}
                                        Some(name) => match icons::by_name(name) {
                                            Some(set) => icons_choice = Some(set),
                                            None => warnings.push(format!(
                                                "{}: [ui] unknown icons value \"{}\" (use \"unicode\", \"ascii\", or \"auto\")",
                                                path.display(),
                                                name
                                            )),
                                        },
                                        None => warnings.push(format!(
                                            "{}: [ui] icons must be a string",
                                            path.display()
                                        )),
                                    }
                                }
                            }
                            None => {
                                warnings.push(format!("{}: [ui] must be a table", path.display()));
                            }
                        }
                    }
                }
                Err(e) => warnings.push(format!("{}: {}", path.display(), e)),
            },
            Err(e) => warnings.push(format!("{}: {}", path.display(), e)),
        }
    }

    Config { keymap, theme, icons: icons_choice, warnings }
}

fn find_config_file() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("SSH_FILES_CONFIG") {
        return Some(PathBuf::from(explicit));
    }

    let xdg_base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| directories::BaseDirs::new().map(|dirs| dirs.home_dir().join(".config")));
    if let Some(base) = xdg_base {
        let path = base.join("ssh-files").join("config.toml");
        if path.is_file() {
            return Some(path);
        }
    }

    if let Some(dirs) = directories::ProjectDirs::from("", "", "ssh-files") {
        let path = dirs.config_dir().join("config.toml");
        if path.is_file() {
            return Some(path);
        }
    }

    None
}
