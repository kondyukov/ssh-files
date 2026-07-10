//! Icon sets used across the UI, kept in one place so every renderer
//! agrees. The unicode set is the default on UTF-8 terminals; the ASCII
//! set covers spartan terminals and is selectable via `[ui] icons` in the
//! config file.

pub struct IconSet {
    pub dir: &'static str,
    pub file: &'static str,
    pub expanded: &'static str,
    pub collapsed: &'static str,
    pub selected: &'static str,
    pub partial: &'static str,
    pub menu_cursor: &'static str,
}

pub static UNICODE: IconSet = IconSet {
    dir: "📁",
    file: "📄",
    expanded: "▼",
    collapsed: "▶",
    selected: "●",
    partial: "◐",
    menu_cursor: "▸",
};

pub static ASCII: IconSet = IconSet {
    dir: "/",
    file: "-",
    expanded: "v",
    collapsed: ">",
    selected: "*",
    partial: "~",
    menu_cursor: ">",
};

/// Look up a set by its config-file name.
pub fn by_name(name: &str) -> Option<&'static IconSet> {
    match name {
        "unicode" | "emoji" => Some(&UNICODE),
        "ascii" => Some(&ASCII),
        _ => None,
    }
}

/// Pick a set from the environment: UTF-8 locales get unicode.
pub fn detect() -> &'static IconSet {
    #[cfg(unix)]
    {
        let locale = std::env::var("LC_ALL")
            .or_else(|_| std::env::var("LC_CTYPE"))
            .or_else(|_| std::env::var("LANG"))
            .unwrap_or_default()
            .to_ascii_lowercase();
        if locale.contains("utf-8") || locale.contains("utf8") {
            &UNICODE
        } else {
            &ASCII
        }
    }
    #[cfg(not(unix))]
    {
        // Windows Terminal and modern consoles render unicode fine.
        &UNICODE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_resolve() {
        assert!(std::ptr::eq(by_name("ascii").unwrap(), &ASCII));
        assert!(std::ptr::eq(by_name("unicode").unwrap(), &UNICODE));
        assert!(by_name("nerd-font").is_none());
    }
}
