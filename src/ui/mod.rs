pub mod geometry;
pub mod icons;
mod tree;

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Gauge, Paragraph, Wrap},
    Frame,
};

use crate::app::{selection_summary, App, Pane};
use crate::file_tree::FileTree;
use crate::input::ContextKind;
use crate::source::FileSource;
use crate::theme::Theme;
use tree::render_file_tree;

/// Truncate a title to fit within max_width, keeping the end (the most
/// specific part of a path) behind an ellipsis. Operates on characters,
/// never byte offsets.
fn truncate_title(title: &str, max_width: u16) -> String {
    let max = max_width as usize;
    let chars: Vec<char> = title.chars().collect();
    if chars.len() <= max {
        title.to_string()
    } else if max <= 4 {
        chars[..max].iter().collect()
    } else {
        let keep = max - 3; // space for "..."
        let tail: String = chars[chars.len() - keep..].iter().collect();
        format!("...{}", tail)
    }
}

/// Standard modal scaffolding: clear the area, render `lines` in a
/// bordered, titled block.
fn render_dialog(frame: &mut Frame, area: Rect, title: &str, border_style: Style, lines: Vec<Line>) {
    let dialog = Paragraph::new(lines)
        .block(
            Block::default()
                .title(title.to_string())
                .borders(Borders::ALL)
                .border_style(border_style),
        )
        .wrap(Wrap { trim: false });

    frame.render_widget(Clear, area);
    frame.render_widget(dialog, area);
}

/// A "[Key] Action" hint row, styled consistently across dialogs.
fn key_hints(theme: &Theme, hints: &[(&str, &str)]) -> Line<'static> {
    let mut spans = Vec::new();
    for (i, (key, action)) in hints.iter().enumerate() {
        let lead = if i == 0 { "  [" } else { "[" };
        spans.push(Span::styled(format!("{}{}]", lead, key), theme.help_key()));
        spans.push(Span::styled(format!(" {}  ", action), theme.help_desc()));
    }
    Line::from(spans)
}

pub fn render(frame: &mut Frame, app: &mut App) {
    let chunks = if app.is_transferring() {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(3),
                Constraint::Length(3),
                Constraint::Length(3),
            ])
            .split(frame.area())
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(3),
                Constraint::Length(3),
            ])
            .split(frame.area())
    };

    render_panes(frame, app, chunks[0]);

    if app.is_transferring() {
        render_progress(frame, app, chunks[1]);
        render_status(frame, app, chunks[2]);
    } else {
        render_status(frame, app, chunks[1]);
    }

    // Overlays render in context stack order: z-order matches input priority.
    let kinds: Vec<ContextKind> = app.input.iter().map(|ctx| ctx.kind()).collect();
    for kind in kinds {
        match kind {
            ContextKind::Browse => {}
            ContextKind::Help => render_help(frame, app),
            ContextKind::CancelConfirm => render_cancel_confirm(frame, app),
            ContextKind::CollisionWarning => render_collision_warning(frame, app),
            ContextKind::OverwriteConfirm => render_overwrite_confirm(frame, app),
            ContextKind::ContextMenu => render_context_menu(frame, app),
            ContextKind::Rename => render_rename_modal(frame, app),
            ContextKind::DeleteConfirm => render_delete_modal(frame, app),
        }
    }
}

/// Pane title from its backing source: "Local: /path" or "user@host: /path".
fn pane_title(source: Option<&dyn FileSource>, root: &str) -> String {
    match source {
        Some(source) => format!(" {}: {} ", source.label(), root),
        None => format!(" {} ", root),
    }
}

fn render_pane(
    frame: &mut Frame,
    area: Rect,
    title: String,
    file_tree: &FileTree,
    focused: bool,
    theme: &Theme,
    icons: &'static icons::IconSet,
) {
    // Borders + padding
    let max_width = area.width.saturating_sub(4);
    let block = Block::default()
        .title(truncate_title(&title, max_width))
        .borders(Borders::ALL)
        .border_style(if focused {
            theme.border_focused()
        } else {
            theme.border_unfocused()
        });

    render_file_tree(frame, area, block, file_tree, focused, theme, icons);
}

fn render_panes(frame: &mut Frame, app: &mut App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    // Store pane areas for mouse hit testing
    app.pane_areas = Some((chunks[0], chunks[1]));

    let left_title = pane_title(app.left_source.as_deref(), &app.local_tree.root_path);
    let right_title = pane_title(app.right_source.as_deref(), &app.remote_tree.root_path);

    render_pane(
        frame,
        chunks[0],
        left_title,
        &app.local_tree,
        app.focus == Pane::Local,
        &app.theme,
        app.icons,
    );
    render_pane(
        frame,
        chunks[1],
        right_title,
        &app.remote_tree,
        app.focus == Pane::Remote,
        &app.theme,
        app.icons,
    );
}

/// Filenames can legally contain control characters (the transfer paths
/// move them byte-exactly), but the terminal must never interpret them:
/// an embedded newline or CR shreds the widget it is printed into, and a
/// raw ESC could smuggle ANSI sequences to the terminal. Every
/// filename-bearing string headed for a widget goes through here.
///
/// `is_control` covers Cc (C0 + DEL + C1, so ESC and the one-byte C1 CSI
/// included). The explicit bidi overrides (U+202A-U+202E, U+2066-U+2069)
/// are neutralized too - they reorder what the user *sees*, the classic
/// name-spoofing trick. Other format characters (ZWNJ/ZWJ) stay: they are
/// legitimate in Persian/Arabic names.
pub(super) fn sanitize_display(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            c if c.is_control() => '\u{FFFD}',
            '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' => '\u{FFFD}',
            c => c,
        })
        .collect()
}

fn render_progress(frame: &mut Frame, app: &App, area: Rect) {
    let theme = &app.theme;

    let progress = app.transfer.as_ref()
        .map(|t| t.overall_progress())
        .unwrap_or(0.0);

    let label = if let Some(ref transfer) = app.transfer {
        if let Some(ref p) = transfer.current_progress {
            format!(
                "{} ({}/{}) - {:.1}%",
                p.filename,
                p.file_index + 1,
                p.total_files,
                progress
            )
        } else {
            format!("{:.1}%", progress)
        }
    } else {
        String::new()
    };

    let gauge = Gauge::default()
        .block(Block::default().borders(Borders::ALL).title(" Progress "))
        .gauge_style(theme.progress_bar())
        .percent(progress as u16)
        .label(sanitize_display(&label));

    frame.render_widget(gauge, area);
}

fn render_status(frame: &mut Frame, app: &App, area: Rect) {
    let theme = &app.theme;

    let (files, dirs) = match app.focus {
        Pane::Local => app.local_tree.selection_counts(),
        Pane::Remote => app.remote_tree.selection_counts(),
    };

    let marked_info = if files + dirs > 0 {
        format!(" | {} selected", selection_summary(files, dirs))
    } else {
        String::new()
    };

    let status_text =
        sanitize_display(&format!(" {}{} | ? for help ", app.status, marked_info));

    let status = Paragraph::new(status_text)
        .style(theme.status())
        .block(Block::default().borders(Borders::ALL));

    frame.render_widget(status, area);
}

fn render_help(frame: &mut Frame, app: &mut App) {
    let theme = &app.theme;
    let area = centered_rect(60, 80, frame.area());

    // Key bindings are derived from the live keymap, so user overrides
    // from the config file show up here automatically.
    let mut help_text: Vec<Line> = Vec::new();
    for (category, entries) in app.keymap.help_sections() {
        if !help_text.is_empty() {
            help_text.push(Line::from(""));
        }
        help_text.push(Line::from(Span::styled(category, theme.help_key())));
        for (keys, label) in entries {
            help_text.push(Line::from(vec![
                Span::styled(format!("  {:<16}", keys), theme.help_key()),
                Span::styled(label, theme.help_desc()),
            ]));
        }
    }

    help_text.push(Line::from(""));
    help_text.push(Line::from(Span::styled("Mouse", theme.help_key())));
    for (input, action) in [
        ("Left click", "Select item"),
        ("Right click", "Context menu"),
        ("Scroll", "Navigate up/down"),
    ] {
        help_text.push(Line::from(vec![
            Span::styled(format!("  {:<16}", input), theme.help_key()),
            Span::styled(action, theme.help_desc()),
        ]));
    }

    // Scroll when the content outgrows the dialog (small terminals). The
    // clamp writes back into the Help context so key handling and render
    // agree; no wrap, so logical lines equal display rows and the scroll
    // math stays exact.
    let viewport = area.height.saturating_sub(2);
    let max_scroll = (help_text.len() as u16).saturating_sub(viewport);
    let border_style = theme.border_focused();
    let scroll = app.clamp_help_scroll(max_scroll, viewport);

    let title = if max_scroll > 0 {
        format!(" Help ({}-{}/{}, ↑↓ scroll) ",
            scroll + 1,
            (scroll + viewport).min(help_text.len() as u16),
            help_text.len(),
        )
    } else {
        String::from(" Help ")
    };

    let dialog = Paragraph::new(help_text)
        .scroll((scroll, 0))
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(border_style),
        );

    frame.render_widget(Clear, area);
    frame.render_widget(dialog, area);
}

fn render_cancel_confirm(frame: &mut Frame, app: &App) {
    let theme = &app.theme;
    let area = centered_rect_fixed(40, 8, frame.area());

    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  Transfer in progress!",
            theme.status().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from("  Cancel transfer and exit?"),
        Line::from(""),
        key_hints(theme, &[("Y", "Yes"), ("N", "No"), ("Esc", "Continue")]),
    ];

    render_dialog(frame, area, " Confirm ", theme.status(), lines);
}

fn render_collision_warning(frame: &mut Frame, app: &App) {
    let Some(collision_files) = app.collision_files() else { return };
    let theme = &app.theme;

    let file_count = collision_files.len().min(5);
    let height = (8 + file_count) as u16;
    let area = centered_rect_fixed(55, height, frame.area());

    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  ⚠ Filename collision detected!",
            theme.status().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from("  Files with same name from different paths:"),
    ];

    for name in collision_files.iter().take(5) {
        lines.push(Line::from(Span::styled(
            format!("    • {}", name),
            theme.file(),
        )));
    }

    if collision_files.len() > 5 {
        lines.push(Line::from(Span::styled(
            format!("    ... and {} more", collision_files.len() - 5),
            theme.dimmed(),
        )));
    }

    lines.push(Line::from(""));
    lines.push(key_hints(theme, &[("Esc", "Close")]));

    render_dialog(frame, area, " Warning ", theme.status(), lines);
}

fn render_overwrite_confirm(frame: &mut Frame, app: &App) {
    let Some(files) = app.overwrite_files() else { return };
    let theme = &app.theme;

    let shown = files.len().min(5);
    let has_more = files.len() > 5;
    let height = (10 + shown + if has_more { 1 } else { 0 }) as u16;
    let area = centered_rect_fixed(60, height, frame.area());

    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("  ⚠ {} file(s) already exist at the destination:", files.len()),
            theme.status().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];

    for name in files.iter().take(5) {
        lines.push(Line::from(Span::styled(
            format!("    • {}", name),
            theme.file(),
        )));
    }

    if has_more {
        lines.push(Line::from(Span::styled(
            format!("    ... and {} more", files.len() - 5),
            theme.dimmed(),
        )));
    }

    lines.push(Line::from(""));
    lines.push(Line::from("  Overwrite them?"));
    lines.push(Line::from(""));
    lines.push(key_hints(theme, &[("Y", "Overwrite"), ("N/Esc", "Cancel")]));

    render_dialog(frame, area, " Confirm Overwrite ", theme.status(), lines);
}

fn render_context_menu(frame: &mut Frame, app: &mut App) {
    let selected_style = app.theme.selected(true);
    let file_style = app.theme.file();
    let border_style = app.theme.border_focused();
    let menu_cursor = app.icons.menu_cursor;

    let screen = frame.area();
    let Some(state) = app.context_menu_state_mut() else { return };

    let max_width = state.items.iter().map(|(_, label)| label.len()).max().unwrap_or(10) + 4;
    let width = (max_width as u16).max(20);
    let height = (state.items.len() + 2) as u16;

    let (mut x, mut y) = state.pos;

    if x + width > screen.width {
        x = screen.width.saturating_sub(width);
    }
    if y + height > screen.height {
        y = screen.height.saturating_sub(height);
    }

    let area = Rect::new(x, y, width, height);

    // Store bounds for mouse hit testing
    state.bounds = Some(area);

    let menu_items: Vec<Line> = state.items.iter().enumerate().map(|(i, (_, label))| {
        let is_selected = i == state.selected;
        let prefix = if is_selected { menu_cursor } else { " " };
        let style = if is_selected { selected_style } else { file_style };
        Line::from(Span::styled(format!("{} {}", prefix, label), style))
    }).collect();

    let menu = Paragraph::new(menu_items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border_style)
        );

    frame.render_widget(Clear, area);
    frame.render_widget(menu, area);
}

fn centered_rect_fixed(width: u16, height: u16, r: Rect) -> Rect {
    let x = r.x + (r.width.saturating_sub(width)) / 2;
    let y = r.y + (r.height.saturating_sub(height)) / 2;
    Rect::new(x, y, width.min(r.width), height.min(r.height))
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

fn render_rename_modal(frame: &mut Frame, app: &App) {
    let state = match app.rename_state() {
        Some(s) => s,
        None => return,
    };

    let theme = &app.theme;
    let width = 50.max(state.input.len() as u16 + 10).min(frame.area().width.saturating_sub(4));
    let area = centered_rect_fixed(width, 8, frame.area());

    // Build input line with cursor
    let input = &state.input;
    let cursor_pos = state.cursor_pos;

    let (before, after) = input.split_at(cursor_pos.min(input.len()));
    let cursor_char = after.chars().next().unwrap_or(' ');
    let after_cursor = if after.len() > 1 { &after[cursor_char.len_utf8()..] } else { "" };

    let input_line = Line::from(vec![
        Span::raw("  "),
        Span::raw(before),
        Span::styled(
            cursor_char.to_string(),
            theme.file().add_modifier(Modifier::REVERSED),
        ),
        Span::raw(after_cursor),
    ]);

    let item_type = if state.is_dir { "directory" } else { "file" };
    let lines = vec![
        Line::from(""),
        Line::from(format!("  Rename {}:", item_type)),
        Line::from(""),
        input_line,
        Line::from(""),
        key_hints(theme, &[("Enter", "Confirm"), ("Esc", "Cancel")]),
    ];

    render_dialog(frame, area, " Rename ", theme.border_focused(), lines);
}

fn render_delete_modal(frame: &mut Frame, app: &App) {
    let state = match app.delete_state() {
        Some(s) => s,
        None => return,
    };

    let theme = &app.theme;

    // Calculate height based on number of items (max 10 shown)
    let items_to_show = state.items.len().min(10);
    let has_more = state.items.len() > 10;
    let height = (8 + items_to_show + if has_more { 1 } else { 0 }) as u16;

    // Calculate width based on longest path
    let max_path_len = state.items.iter()
        .take(10)
        .map(|i| i.relative_path.len())
        .max()
        .unwrap_or(20);
    let width = (max_path_len as u16 + 12).max(45).min(frame.area().width.saturating_sub(4));

    let area = centered_rect_fixed(width, height, frame.area());

    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("  Delete {} item(s)?", state.items.len()),
            theme.file().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];

    // Add file paths
    for item in state.items.iter().take(10) {
        let icon = if item.is_dir { app.icons.dir } else { app.icons.file };
        lines.push(Line::from(format!("  {} {}", icon, item.relative_path)));
    }

    if has_more {
        lines.push(Line::from(Span::styled(
            format!("  ... and {} more", state.items.len() - 10),
            theme.help_desc(),
        )));
    }

    lines.push(Line::from(""));
    lines.push(key_hints(theme, &[("Y", "Delete"), ("N/Esc", "Cancel")]));

    render_dialog(frame, area, " Confirm Delete ", theme.status(), lines);
}

#[cfg(test)]
mod sanitize_tests {
    use super::sanitize_display;

    #[test]
    fn hostile_names_render_inert() {
        assert_eq!(sanitize_display("plain héllo📁.txt"), "plain héllo📁.txt");
        assert_eq!(sanitize_display("new\nline.txt"), "new\u{FFFD}line.txt");
        assert_eq!(sanitize_display("cr\rname.txt"), "cr\u{FFFD}name.txt");
        // ESC and the single-char C1 CSI can start ANSI sequences.
        assert_eq!(sanitize_display("\x1b[31mred"), "\u{FFFD}[31mred");
        assert_eq!(sanitize_display("\u{9b}31mred"), "\u{FFFD}31mred");
        // RLO reverses displayed order: "gpj.exe" spoofing.
        assert_eq!(sanitize_display("x\u{202E}gpj.exe"), "x\u{FFFD}gpj.exe");
        // Persian ZWNJ is a legitimate name character and must survive.
        assert_eq!(sanitize_display("می\u{200C}خواهم"), "می\u{200C}خواهم");
    }
}
