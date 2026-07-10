use ratatui::{
    layout::Rect,
    text::{Line, Span},
    widgets::{Block, Paragraph},
    Frame,
};
use humansize::{format_size, BINARY};
use std::ops::ControlFlow;

use super::geometry;
use super::icons::IconSet;
use crate::file_tree::{FileTree, RowView, SelectionState};
use crate::theme::Theme;

pub fn render_file_tree(
    frame: &mut Frame,
    area: Rect,
    block: Block,
    tree: &FileTree,
    focused: bool,
    theme: &Theme,
    icons: &'static IconSet,
) {
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Scrolling must agree with mouse hit-testing; both use ui::geometry.
    let visible_height = geometry::visible_height(area);
    let scroll_offset = geometry::scroll_offset(tree.cursor, visible_height);

    // Walk visible rows only as far as the end of the on-screen window and
    // materialize just the rows inside it - a huge expanded tree costs the
    // frame nothing beyond its scroll position.
    let mut lines: Vec<Line> = Vec::with_capacity(visible_height);
    let mut pos = 0usize;
    tree.walk_visible(&mut |index, depth| {
        if pos >= scroll_offset + visible_height {
            return ControlFlow::Break(());
        }
        if pos >= scroll_offset {
            let node = tree.row_view(index, depth);
            lines.push(render_row(&node, pos == tree.cursor, focused, theme, icons));
        }
        pos += 1;
        ControlFlow::Continue(())
    });

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}

fn render_row(
    node: &RowView,
    is_cursor: bool,
    focused: bool,
    theme: &Theme,
    icons: &'static IconSet,
) -> Line<'static> {
    let is_selected = matches!(node.selected, SelectionState::Selected | SelectionState::Inherited);
    let is_partial = matches!(node.selected, SelectionState::Partial);

    // Build indentation
    let indent: String = "  ".repeat(node.depth);

    // Expand indicator
    let expand_indicator = if node.is_dir {
        if node.expanded { icons.expanded } else { icons.collapsed }
    } else {
        " "
    };

    // Icon
    let icon = if node.is_dir { icons.dir } else { icons.file };

    // Selection indicator
    let mark = if is_selected {
        icons.selected
    } else if is_partial {
        icons.partial
    } else {
        " "
    };

    // Size for files
    let size_str = if node.is_dir {
        String::new()
    } else {
        format!("  {}", format_size(node.size, BINARY))
    };

    // Build the line
    let base_style = if is_cursor {
        theme.selected(focused)
    } else if node.is_dir {
        theme.directory()
    } else {
        theme.file()
    };

    let mark_style = if is_selected || is_partial {
        theme.marked()
    } else {
        base_style
    };

    let mut spans = vec![
        Span::styled(format!("{} ", mark), mark_style),
        Span::styled(indent, base_style),
        Span::styled(format!("{} ", expand_indicator), base_style),
        Span::styled(format!("{} ", icon), base_style),
        Span::styled(node.name.to_string(), base_style),
    ];

    if !node.is_dir {
        let size_style = if is_cursor {
            theme.selected(focused)
        } else {
            theme.size()
        };
        spans.push(Span::styled(size_str, size_style));
    }

    if node.is_dir && is_cursor {
        spans.push(Span::styled("/".to_string(), base_style));
    }

    Line::from(spans)
}
