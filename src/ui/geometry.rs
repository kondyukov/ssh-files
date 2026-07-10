//! Shared pane geometry.
//!
//! The tree renderer and the mouse hit-testing in app.rs must agree on
//! these formulas exactly, or clicks land on the wrong rows. Every
//! scroll/row calculation goes through here; nothing may inline it.

use ratatui::layout::Rect;

/// Rows available for tree content inside a bordered pane.
pub fn visible_height(area: Rect) -> usize {
    area.height.saturating_sub(2) as usize
}

/// First visible row given the cursor position: the view scrolls just
/// enough to keep the cursor on the last row.
pub fn scroll_offset(cursor: usize, visible_height: usize) -> usize {
    if visible_height > 0 && cursor >= visible_height {
        cursor - visible_height + 1
    } else {
        0
    }
}

/// The visible-row index under screen row `y` in a bordered pane, given
/// the tree cursor (which determines scrolling). Callers bounds-check the
/// result against the tree's visible count.
pub fn row_at(area: Rect, cursor: usize, y: u16) -> usize {
    let inner_y = y.saturating_sub(area.y + 1) as usize;
    scroll_offset(cursor, visible_height(area)) + inner_y
}

/// Whether a point is inside `area` (borders included).
pub fn contains(area: Rect, x: u16, y: u16) -> bool {
    x >= area.x && x < area.x + area.width && y >= area.y && y < area.y + area.height
}

/// Whether a point is strictly inside `area`'s borders (content cells only).
pub fn contains_inner(area: Rect, x: u16, y: u16) -> bool {
    x > area.x
        && x + 1 < area.x + area.width
        && y > area.y
        && y + 1 < area.y + area.height
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrolling_keeps_cursor_in_view() {
        assert_eq!(scroll_offset(0, 10), 0);
        assert_eq!(scroll_offset(9, 10), 0);
        assert_eq!(scroll_offset(10, 10), 1);
        assert_eq!(scroll_offset(25, 10), 16);
        assert_eq!(scroll_offset(5, 0), 0);
    }

    #[test]
    fn row_at_accounts_for_border_and_scroll() {
        let area = Rect::new(0, 0, 40, 12); // 10 content rows
        // Unscrolled: first content row is y=1.
        assert_eq!(row_at(area, 0, 1), 0);
        assert_eq!(row_at(area, 0, 10), 9);
        // Cursor at 15 scrolls by 6.
        assert_eq!(row_at(area, 15, 1), 6);
        assert_eq!(row_at(area, 15, 10), 15);
    }

    #[test]
    fn containment_rules() {
        let area = Rect::new(5, 5, 10, 4);
        assert!(contains(area, 5, 5));
        assert!(contains(area, 14, 8));
        assert!(!contains(area, 15, 8));

        // Inner excludes the border ring.
        assert!(!contains_inner(area, 5, 5));
        assert!(contains_inner(area, 6, 6));
        assert!(!contains_inner(area, 14, 6));
    }
}
