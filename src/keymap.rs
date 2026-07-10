//! Declarative key bindings for the Browse context.
//!
//! The `ACTIONS` registry is the single source of truth: it defines the
//! config-file name, the dispatched `Action`, and the category/label used
//! to build the help screen. Default bindings can be overridden per-key
//! from the `[keys]` table of the user config file (see `config.rs`).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::action::{Action, TransferMode};
use crate::app::Pane;
use crate::transfer::TransferDirection;

/// A normalized key chord: the key plus ctrl/alt (and shift for non-char keys).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyCombo {
    pub code: KeyCode,
    pub mods: KeyModifiers,
}

impl KeyCombo {
    pub fn from_event(event: KeyEvent) -> Self {
        let mut mods = event.modifiers & (KeyModifiers::CONTROL | KeyModifiers::ALT);
        // For character keys shift is already encoded in the character itself
        // ('G' vs 'g'); keep it only for named keys (e.g. shift+F5).
        if !matches!(event.code, KeyCode::Char(_)) {
            mods |= event.modifiers & KeyModifiers::SHIFT;
        }
        Self { code: event.code, mods }
    }

    /// Parse a combo like "g", "G", "ctrl+c", "F5", "space", "shift+f5".
    pub fn parse(s: &str) -> Option<Self> {
        let mut mods = KeyModifiers::NONE;
        let mut key = s.trim();

        while let Some((prefix, rest)) = key.split_once('+') {
            match prefix.trim().to_ascii_lowercase().as_str() {
                "ctrl" | "control" => mods |= KeyModifiers::CONTROL,
                "alt" => mods |= KeyModifiers::ALT,
                "shift" => mods |= KeyModifiers::SHIFT,
                _ => return None,
            }
            key = rest.trim();
        }

        let code = match key.to_ascii_lowercase().as_str() {
            "" => return None,
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "enter" | "return" => KeyCode::Enter,
            "esc" | "escape" => KeyCode::Esc,
            "tab" => KeyCode::Tab,
            "backtab" => KeyCode::BackTab,
            "backspace" => KeyCode::Backspace,
            "delete" | "del" => KeyCode::Delete,
            "insert" => KeyCode::Insert,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "pageup" => KeyCode::PageUp,
            "pagedown" => KeyCode::PageDown,
            "space" => KeyCode::Char(' '),
            lower => {
                if let Some(n) = lower.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
                    if (1..=12).contains(&n) {
                        KeyCode::F(n)
                    } else {
                        return None;
                    }
                } else {
                    let mut chars = key.chars();
                    let c = chars.next()?;
                    if chars.next().is_some() {
                        return None;
                    }
                    // Ctrl/alt chords arrive from crossterm with lowercase chars.
                    if mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
                        KeyCode::Char(c.to_ascii_lowercase())
                    } else {
                        KeyCode::Char(c)
                    }
                }
            }
        };

        // Shift on a character key is encoded in the character itself.
        if matches!(code, KeyCode::Char(_)) {
            mods.remove(KeyModifiers::SHIFT);
        }

        Some(Self { code, mods })
    }
}

impl std::fmt::Display for KeyCombo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.mods.contains(KeyModifiers::CONTROL) {
            write!(f, "Ctrl+")?;
        }
        if self.mods.contains(KeyModifiers::ALT) {
            write!(f, "Alt+")?;
        }
        if self.mods.contains(KeyModifiers::SHIFT) {
            write!(f, "Shift+")?;
        }
        match self.code {
            KeyCode::Up => write!(f, "↑"),
            KeyCode::Down => write!(f, "↓"),
            KeyCode::Left => write!(f, "←"),
            KeyCode::Right => write!(f, "→"),
            KeyCode::Enter => write!(f, "Enter"),
            KeyCode::Esc => write!(f, "Esc"),
            KeyCode::Tab => write!(f, "Tab"),
            KeyCode::BackTab => write!(f, "BackTab"),
            KeyCode::Backspace => write!(f, "Backspace"),
            KeyCode::Delete => write!(f, "Del"),
            KeyCode::Insert => write!(f, "Ins"),
            KeyCode::Home => write!(f, "Home"),
            KeyCode::End => write!(f, "End"),
            KeyCode::PageUp => write!(f, "PgUp"),
            KeyCode::PageDown => write!(f, "PgDn"),
            KeyCode::F(n) => write!(f, "F{}", n),
            KeyCode::Char(' ') => write!(f, "Space"),
            KeyCode::Char(c) => write!(f, "{}", c),
            other => write!(f, "{:?}", other),
        }
    }
}

pub struct ActionInfo {
    pub name: &'static str,
    pub action: Action,
    pub category: &'static str,
    pub label: &'static str,
}

/// Every action bindable from the config file, in help-screen order.
pub const ACTIONS: &[ActionInfo] = &[
    ActionInfo { name: "move_up", action: Action::MoveUp, category: "Navigation", label: "Move up" },
    ActionInfo { name: "move_down", action: Action::MoveDown, category: "Navigation", label: "Move down" },
    ActionInfo { name: "page_up", action: Action::PageUp, category: "Navigation", label: "Page up" },
    ActionInfo { name: "page_down", action: Action::PageDown, category: "Navigation", label: "Page down" },
    ActionInfo { name: "go_to_top", action: Action::GoToTop, category: "Navigation", label: "Go to top" },
    ActionInfo { name: "go_to_bottom", action: Action::GoToBottom, category: "Navigation", label: "Go to bottom" },
    ActionInfo { name: "focus_left", action: Action::FocusPane(Pane::Local), category: "Navigation", label: "Focus left pane" },
    ActionInfo { name: "focus_right", action: Action::FocusPane(Pane::Remote), category: "Navigation", label: "Focus right pane" },
    ActionInfo { name: "enter_dir", action: Action::EnterDir, category: "Navigation", label: "Enter directory" },
    ActionInfo { name: "go_up", action: Action::GoUp, category: "Navigation", label: "Go up one level" },
    ActionInfo { name: "toggle_expand", action: Action::ToggleExpand, category: "Navigation", label: "Expand/collapse directory" },
    ActionInfo { name: "toggle_select", action: Action::ToggleSelect, category: "Selection", label: "Toggle select" },
    ActionInfo { name: "select_all", action: Action::SelectAll, category: "Selection", label: "Select all" },
    ActionInfo { name: "deselect_all", action: Action::DeselectAll, category: "Selection", label: "Deselect all" },
    // Send labels match the context menu: the arrow points at the
    // receiving pane as laid out on screen. "Download"/"upload" survive as
    // the config action names for compatibility and familiarity.
    ActionInfo {
        name: "download_flat",
        action: Action::Transfer { direction: TransferDirection::Download, mode: TransferMode::Flat },
        category: "Transfer",
        label: "Send flat <- (right pane to left)",
    },
    ActionInfo {
        name: "download_preserve",
        action: Action::Transfer { direction: TransferDirection::Download, mode: TransferMode::Preserve },
        category: "Transfer",
        label: "Send tree <- (right pane to left)",
    },
    ActionInfo {
        name: "upload_flat",
        action: Action::Transfer { direction: TransferDirection::Upload, mode: TransferMode::Flat },
        category: "Transfer",
        label: "Send flat -> (left pane to right)",
    },
    ActionInfo {
        name: "upload_preserve",
        action: Action::Transfer { direction: TransferDirection::Upload, mode: TransferMode::Preserve },
        category: "Transfer",
        label: "Send tree -> (left pane to right)",
    },
    // Clipboard (copy/cut/paste) is deferred past this release; restore
    // these entries when it lands completely and cleanly.
    //
    // ActionInfo { name: "copy", action: Action::ClipboardCopy, category: "Clipboard", label: "Copy" },
    // ActionInfo { name: "cut", action: Action::ClipboardCut, category: "Clipboard", label: "Cut" },
    // ActionInfo { name: "paste", action: Action::Paste, category: "Clipboard", label: "Paste" },
    ActionInfo { name: "copy_path", action: Action::CopyPath, category: "Clipboard", label: "Copy path" },
    ActionInfo { name: "rename", action: Action::StartRename, category: "File Operations", label: "Rename" },
    ActionInfo { name: "delete", action: Action::StartDelete, category: "File Operations", label: "Delete" },
    ActionInfo { name: "context_menu", action: Action::OpenContextMenu, category: "File Operations", label: "Open context menu" },
    ActionInfo { name: "toggle_hidden", action: Action::ToggleHidden, category: "Other", label: "Show/hide hidden files" },
    ActionInfo { name: "refresh", action: Action::Refresh, category: "Other", label: "Refresh" },
    ActionInfo { name: "toggle_help", action: Action::ToggleHelp, category: "Other", label: "Toggle help" },
    ActionInfo { name: "quit", action: Action::Quit, category: "Other", label: "Quit" },
];

fn action_by_name(name: &str) -> Option<Action> {
    ACTIONS.iter().find(|info| info.name == name).map(|info| info.action)
}

/// Key bindings for the Browse context.
pub struct Keymap {
    bindings: Vec<(KeyCombo, Action)>,
}

impl Default for Keymap {
    fn default() -> Self {
        let defaults: &[(&str, &str)] = &[
            ("up", "move_up"),
            ("k", "move_up"),
            ("down", "move_down"),
            ("j", "move_down"),
            ("pageup", "page_up"),
            ("pagedown", "page_down"),
            ("g", "go_to_top"),
            ("G", "go_to_bottom"),
            ("left", "focus_left"),
            ("right", "focus_right"),
            ("enter", "enter_dir"),
            ("l", "enter_dir"),
            ("backspace", "go_up"),
            ("h", "go_up"),
            ("tab", "toggle_expand"),
            ("space", "toggle_select"),
            ("a", "select_all"),
            ("A", "deselect_all"),
            ("d", "download_flat"),
            ("y", "download_flat"),
            ("u", "upload_flat"),
            ("F2", "rename"),
            ("delete", "delete"),
            ("x", "delete"),
            ("m", "context_menu"),
            (".", "toggle_hidden"),
            ("r", "refresh"),
            ("F5", "refresh"),
            ("?", "toggle_help"),
            ("q", "quit"),
            ("ctrl+c", "quit"),
        ];

        let bindings = defaults
            .iter()
            .map(|(combo, action)| {
                (
                    KeyCombo::parse(combo).expect("default combo parses"),
                    action_by_name(action).expect("default action exists"),
                )
            })
            .collect();

        Self { bindings }
    }
}

impl Keymap {
    pub fn lookup(&self, event: KeyEvent) -> Option<Action> {
        let combo = KeyCombo::from_event(event);
        self.bindings
            .iter()
            .find(|(bound, _)| *bound == combo)
            .map(|(_, action)| *action)
    }

    /// Apply `[keys]` overrides from the config file. Each entry rebinds a
    /// key combo to a named action; "none" unbinds it. Returns warnings for
    /// entries that could not be applied.
    pub fn apply(&mut self, keys: &toml::Table) -> Vec<String> {
        let mut warnings = Vec::new();

        for (combo_str, value) in keys {
            let Some(combo) = KeyCombo::parse(combo_str) else {
                warnings.push(format!("invalid key combo \"{}\"", combo_str));
                continue;
            };
            let Some(action_name) = value.as_str() else {
                warnings.push(format!("\"{}\": action must be a string", combo_str));
                continue;
            };

            if action_name == "none" {
                self.bindings.retain(|(bound, _)| *bound != combo);
                continue;
            }

            let Some(action) = action_by_name(action_name) else {
                warnings.push(format!(
                    "\"{}\": unknown action \"{}\"",
                    combo_str, action_name
                ));
                continue;
            };

            self.bindings.retain(|(bound, _)| *bound != combo);
            self.bindings.push((combo, action));
        }

        warnings
    }

    /// Help-screen content: categories in registry order, each action with
    /// its bound keys joined by "/". Unbound actions are omitted.
    pub fn help_sections(&self) -> Vec<(&'static str, Vec<(String, &'static str)>)> {
        let mut sections: Vec<(&'static str, Vec<(String, &'static str)>)> = Vec::new();

        for info in ACTIONS {
            let keys: Vec<String> = self
                .bindings
                .iter()
                .filter(|(_, action)| *action == info.action)
                .map(|(combo, _)| combo.to_string())
                .collect();
            if keys.is_empty() {
                continue;
            }

            let entry = (keys.join("/"), info.label);
            match sections.last_mut() {
                Some((category, entries)) if *category == info.category => entries.push(entry),
                _ => sections.push((info.category, vec![entry])),
            }
        }

        sections
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn parse_combos() {
        assert_eq!(
            KeyCombo::parse("ctrl+c"),
            Some(KeyCombo { code: KeyCode::Char('c'), mods: KeyModifiers::CONTROL })
        );
        assert_eq!(
            KeyCombo::parse("G"),
            Some(KeyCombo { code: KeyCode::Char('G'), mods: KeyModifiers::NONE })
        );
        assert_eq!(
            KeyCombo::parse("F5"),
            Some(KeyCombo { code: KeyCode::F(5), mods: KeyModifiers::NONE })
        );
        assert_eq!(
            KeyCombo::parse("space"),
            Some(KeyCombo { code: KeyCode::Char(' '), mods: KeyModifiers::NONE })
        );
        assert_eq!(
            KeyCombo::parse("shift+f5"),
            Some(KeyCombo { code: KeyCode::F(5), mods: KeyModifiers::SHIFT })
        );
        assert_eq!(KeyCombo::parse(""), None);
        assert_eq!(KeyCombo::parse("hyper+x"), None);
        assert_eq!(KeyCombo::parse("notakey"), None);
    }

    #[test]
    fn default_lookup() {
        let keymap = Keymap::default();
        assert_eq!(
            keymap.lookup(key(KeyCode::Char('q'), KeyModifiers::NONE)),
            Some(Action::Quit)
        );
        assert_eq!(
            keymap.lookup(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(Action::Quit)
        );
        // Shift is implied by the uppercase char itself.
        assert_eq!(
            keymap.lookup(key(KeyCode::Char('G'), KeyModifiers::SHIFT)),
            Some(Action::GoToBottom)
        );
        assert_eq!(keymap.lookup(key(KeyCode::Char('z'), KeyModifiers::NONE)), None);
    }

    #[test]
    fn apply_overrides() {
        let mut keymap = Keymap::default();
        let overrides: toml::Table = r#"
            "ctrl+d" = "download_preserve"
            "q" = "none"
            "F9" = "frobnicate"
            "bad++key" = "quit"
        "#
        .parse()
        .unwrap();

        let warnings = keymap.apply(&overrides);
        assert_eq!(warnings.len(), 2, "{:?}", warnings);

        assert_eq!(
            keymap.lookup(key(KeyCode::Char('d'), KeyModifiers::CONTROL)),
            Some(Action::Transfer {
                direction: TransferDirection::Download,
                mode: TransferMode::Preserve
            })
        );
        // "q" unbound; ctrl+c still quits.
        assert_eq!(keymap.lookup(key(KeyCode::Char('q'), KeyModifiers::NONE)), None);
        assert_eq!(
            keymap.lookup(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(Action::Quit)
        );
    }

    #[test]
    fn rebind_replaces_existing() {
        let mut keymap = Keymap::default();
        let overrides: toml::Table = r#""d" = "delete""#.parse().unwrap();
        assert!(keymap.apply(&overrides).is_empty());
        assert_eq!(
            keymap.lookup(key(KeyCode::Char('d'), KeyModifiers::NONE)),
            Some(Action::StartDelete)
        );
    }

    #[test]
    fn help_sections_cover_defaults() {
        let sections = Keymap::default().help_sections();
        let categories: Vec<_> = sections.iter().map(|(c, _)| *c).collect();
        assert_eq!(
            categories,
            vec!["Navigation", "Selection", "Transfer", "File Operations", "Other"]
        );
        // Unbound-by-default clipboard actions don't appear.
        assert!(!categories.contains(&"Clipboard"));

        let nav = &sections[0].1;
        assert!(nav.iter().any(|(keys, label)| keys == "↑/k" && *label == "Move up"));
    }
}
