use ratatui::style::{Color, Modifier, Style};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorSupport {
    TrueColor,
    Palette256,
    Ansi,
    None,
}

impl ColorSupport {
    pub fn detect() -> Self {
        if std::env::var("NO_COLOR").is_ok() {
            return Self::None;
        }

        if let Ok(colorterm) = std::env::var("COLORTERM") {
            if colorterm == "truecolor" || colorterm == "24bit" {
                return Self::TrueColor;
            }
        }

        // Windows Terminal
        if std::env::var("WT_SESSION").is_ok() {
            return Self::TrueColor;
        }

        if let Ok(term) = std::env::var("TERM") {
            if term.contains("256color") {
                return Self::Palette256;
            }
            if term.contains("color") || term == "xterm" {
                return Self::Ansi;
            }
        }

        Self::Ansi
    }
}

/// User color overrides from the `[theme]` config table, validated at load
/// time and applied on top of the capability-selected palette.
#[derive(Default)]
pub struct ThemeOverrides {
    colors: Vec<(String, Color)>,
}

impl ThemeOverrides {
    pub fn from_table(table: &toml::Table) -> (Self, Vec<String>) {
        let mut colors = Vec::new();
        let mut warnings = Vec::new();
        // Scratch palette used only to validate slot names.
        let mut scratch = ThemeColors::ansi();

        for (slot, value) in table {
            match parse_color(value) {
                Ok(color) => {
                    if scratch.set(slot, color) {
                        colors.push((slot.clone(), color));
                    } else {
                        warnings.push(format!("unknown color slot \"{}\"", slot));
                    }
                }
                Err(e) => warnings.push(format!("\"{}\": {}", slot, e)),
            }
        }

        (Self { colors }, warnings)
    }
}

fn parse_color(value: &toml::Value) -> Result<Color, String> {
    match value {
        toml::Value::Integer(i) if (0..=255).contains(i) => Ok(Color::Indexed(*i as u8)),
        toml::Value::Integer(i) => Err(format!("palette index {} out of range 0-255", i)),
        toml::Value::String(s) => parse_color_str(s),
        other => Err(format!(
            "expected color string or palette index, got {}",
            other.type_str()
        )),
    }
}

fn parse_color_str(s: &str) -> Result<Color, String> {
    let lower = s.trim().to_ascii_lowercase();

    if let Some(hex) = lower.strip_prefix('#') {
        if hex.len() == 6 {
            if let (Ok(r), Ok(g), Ok(b)) = (
                u8::from_str_radix(&hex[0..2], 16),
                u8::from_str_radix(&hex[2..4], 16),
                u8::from_str_radix(&hex[4..6], 16),
            ) {
                return Ok(Color::Rgb(r, g, b));
            }
        }
        return Err(format!("invalid hex color \"{}\"", s));
    }

    if let Ok(idx) = lower.parse::<u8>() {
        return Ok(Color::Indexed(idx));
    }

    match lower.as_str() {
        "black" => Ok(Color::Black),
        "red" => Ok(Color::Red),
        "green" => Ok(Color::Green),
        "yellow" => Ok(Color::Yellow),
        "blue" => Ok(Color::Blue),
        "magenta" => Ok(Color::Magenta),
        "cyan" => Ok(Color::Cyan),
        "gray" | "grey" => Ok(Color::Gray),
        "darkgray" | "darkgrey" => Ok(Color::DarkGray),
        "lightred" => Ok(Color::LightRed),
        "lightgreen" => Ok(Color::LightGreen),
        "lightyellow" => Ok(Color::LightYellow),
        "lightblue" => Ok(Color::LightBlue),
        "lightmagenta" => Ok(Color::LightMagenta),
        "lightcyan" => Ok(Color::LightCyan),
        "white" => Ok(Color::White),
        "reset" => Ok(Color::Reset),
        _ => Err(format!("unknown color \"{}\"", s)),
    }
}

#[derive(Debug, Clone)]
pub struct ThemeColors {
    pub border_focused: Color,
    pub border_unfocused: Color,
    pub directory: Color,
    pub file: Color,
    pub selected_bg: Color,
    pub selected_fg: Color,
    pub marked_indicator: Color,
    pub status_text: Color,
    pub size: Color,
    pub help_key: Color,
    pub help_desc: Color,
    pub dimmed: Color,
}

impl ThemeColors {
    fn truecolor() -> Self {
        Self {
            border_focused: Color::Rgb(97, 175, 239),
            border_unfocused: Color::Rgb(92, 99, 112),
            directory: Color::Rgb(97, 175, 239),
            file: Color::Rgb(171, 178, 191),
            selected_bg: Color::Rgb(62, 68, 81),
            selected_fg: Color::Rgb(224, 224, 224),
            marked_indicator: Color::Rgb(152, 195, 121),
            status_text: Color::Rgb(229, 192, 123),
            size: Color::Rgb(152, 195, 121),
            help_key: Color::Rgb(198, 120, 221),
            help_desc: Color::Rgb(171, 178, 191),
            dimmed: Color::Rgb(92, 99, 112),
        }
    }

    fn palette256() -> Self {
        Self {
            border_focused: Color::Indexed(75),
            border_unfocused: Color::Indexed(240),
            directory: Color::Indexed(75),
            file: Color::Indexed(250),
            selected_bg: Color::Indexed(238),
            selected_fg: Color::Indexed(255),
            marked_indicator: Color::Indexed(114),
            status_text: Color::Indexed(220),
            size: Color::Indexed(114),
            help_key: Color::Indexed(176),
            help_desc: Color::Indexed(250),
            dimmed: Color::Indexed(240),
        }
    }

    fn ansi() -> Self {
        Self {
            border_focused: Color::Cyan,
            border_unfocused: Color::DarkGray,
            directory: Color::Blue,
            file: Color::White,
            selected_bg: Color::Blue,
            selected_fg: Color::White,
            marked_indicator: Color::Green,
            status_text: Color::Yellow,
            size: Color::Green,
            help_key: Color::Magenta,
            help_desc: Color::White,
            dimmed: Color::DarkGray,
        }
    }

    fn none() -> Self {
        Self {
            border_focused: Color::Reset,
            border_unfocused: Color::Reset,
            directory: Color::Reset,
            file: Color::Reset,
            selected_bg: Color::Reset,
            selected_fg: Color::Reset,
            marked_indicator: Color::Reset,
            status_text: Color::Reset,
            size: Color::Reset,
            help_key: Color::Reset,
            help_desc: Color::Reset,
            dimmed: Color::Reset,
        }
    }

    /// Set a color slot by its config name. Returns false for unknown slots.
    fn set(&mut self, slot: &str, color: Color) -> bool {
        let target = match slot {
            "border_focused" => &mut self.border_focused,
            "border_unfocused" => &mut self.border_unfocused,
            "directory" => &mut self.directory,
            "file" => &mut self.file,
            "selected_bg" => &mut self.selected_bg,
            "selected_fg" => &mut self.selected_fg,
            "marked_indicator" => &mut self.marked_indicator,
            "status_text" => &mut self.status_text,
            "size" => &mut self.size,
            "help_key" => &mut self.help_key,
            "help_desc" => &mut self.help_desc,
            "dimmed" => &mut self.dimmed,
            _ => return false,
        };
        *target = color;
        true
    }
}

pub struct Theme {
    pub capability: ColorSupport,
    pub colors: ThemeColors,
}

impl Theme {
    pub fn auto() -> Self {
        Self::with_capability(ColorSupport::detect())
    }

    pub fn with_capability(cap: ColorSupport) -> Self {
        let colors = match cap {
            ColorSupport::TrueColor => ThemeColors::truecolor(),
            ColorSupport::Palette256 => ThemeColors::palette256(),
            ColorSupport::Ansi => ThemeColors::ansi(),
            ColorSupport::None => ThemeColors::none(),
        };
        Self { capability: cap, colors }
    }

    /// Apply user color overrides on top of the capability palette.
    /// Skipped entirely when colors are disabled (NO_COLOR / --color none).
    pub fn apply_overrides(&mut self, overrides: &ThemeOverrides) {
        if self.capability == ColorSupport::None {
            return;
        }
        for (slot, color) in &overrides.colors {
            self.colors.set(slot, *color);
        }
    }

    pub fn border_focused(&self) -> Style {
        Style::default().fg(self.colors.border_focused)
    }

    pub fn border_unfocused(&self) -> Style {
        Style::default().fg(self.colors.border_unfocused)
    }

    pub fn selected(&self, focused: bool) -> Style {
        if focused {
            Style::default()
                .bg(self.colors.selected_bg)
                .fg(self.colors.selected_fg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().add_modifier(Modifier::UNDERLINED)
        }
    }

    pub fn directory(&self) -> Style {
        Style::default().fg(self.colors.directory)
    }

    pub fn file(&self) -> Style {
        Style::default().fg(self.colors.file)
    }

    pub fn size(&self) -> Style {
        Style::default().fg(self.colors.size)
    }

    pub fn marked(&self) -> Style {
        Style::default().fg(self.colors.marked_indicator)
    }

    pub fn status(&self) -> Style {
        Style::default().fg(self.colors.status_text)
    }

    pub fn progress_bar(&self) -> Style {
        Style::default()
            .fg(self.colors.marked_indicator)
            .bg(self.colors.dimmed)
    }

    pub fn help_key(&self) -> Style {
        Style::default()
            .fg(self.colors.help_key)
            .add_modifier(Modifier::BOLD)
    }

    pub fn help_desc(&self) -> Style {
        Style::default().fg(self.colors.help_desc)
    }

    pub fn dimmed(&self) -> Style {
        Style::default().fg(self.colors.dimmed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_colors() {
        assert_eq!(parse_color_str("#61afef"), Ok(Color::Rgb(0x61, 0xaf, 0xef)));
        assert_eq!(parse_color_str("white"), Ok(Color::White));
        assert_eq!(parse_color_str("DarkGrey"), Ok(Color::DarkGray));
        assert_eq!(parse_color_str("114"), Ok(Color::Indexed(114)));
        assert!(parse_color_str("#xyz").is_err());
        assert!(parse_color_str("#61afef00").is_err());
        assert!(parse_color_str("mauve-ish").is_err());

        assert_eq!(
            parse_color(&toml::Value::Integer(75)),
            Ok(Color::Indexed(75))
        );
        assert!(parse_color(&toml::Value::Integer(300)).is_err());
        assert!(parse_color(&toml::Value::Boolean(true)).is_err());
    }

    #[test]
    fn overrides_from_table() {
        let table: toml::Table = r##"
            directory = "#ff0000"
            border_focused = 75
            not_a_slot = "red"
            file = "not-a-color"
        "##
        .parse()
        .unwrap();

        let (overrides, warnings) = ThemeOverrides::from_table(&table);
        assert_eq!(warnings.len(), 2, "{:?}", warnings);

        let mut theme = Theme::with_capability(ColorSupport::TrueColor);
        theme.apply_overrides(&overrides);
        assert_eq!(theme.colors.directory, Color::Rgb(0xff, 0, 0));
        assert_eq!(theme.colors.border_focused, Color::Indexed(75));
    }

    #[test]
    fn overrides_respect_no_color() {
        let table: toml::Table = r#"directory = "red""#.parse().unwrap();
        let (overrides, _) = ThemeOverrides::from_table(&table);

        let mut theme = Theme::with_capability(ColorSupport::None);
        theme.apply_overrides(&overrides);
        assert_eq!(theme.colors.directory, Color::Reset);
    }
}
