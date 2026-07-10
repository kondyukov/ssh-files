use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;

use crate::action::Action;
use crate::app::{App, DeleteState, PendingTransfer, RenameState};

/// State for an open context menu. Items are snapshotted when the menu
/// opens (the pane it was opened on becomes the focused pane); bounds are
/// recorded at render time for mouse hit testing.
pub struct ContextMenuState {
    pub pos: (u16, u16),
    pub bounds: Option<Rect>,
    pub selected: usize,
    pub items: Vec<(Action, String)>,
}

/// A widget that owns input. The top of the `ContextStack` receives events;
/// each modal's state lives inside its variant.
pub enum InputContext {
    Browse,
    /// Help modal. `scroll` is the first visible line; `viewport` is the
    /// content height recorded at render time (for page scrolling and
    /// clamping), mirroring how the context menu records its bounds.
    Help { scroll: u16, viewport: u16 },
    ContextMenu(ContextMenuState),
    Rename(RenameState),
    DeleteConfirm(DeleteState),
    CancelConfirm,
    CollisionWarning { files: Vec<String> },
    /// A prepared transfer would overwrite existing destination files;
    /// one confirmation covers the whole batch.
    OverwriteConfirm {
        files: Vec<String>,
        pending: PendingTransfer,
    },
}

/// Discriminant of `InputContext`, for routing without holding a borrow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextKind {
    Browse,
    Help,
    ContextMenu,
    Rename,
    DeleteConfirm,
    CancelConfirm,
    CollisionWarning,
    OverwriteConfirm,
}

impl InputContext {
    pub fn kind(&self) -> ContextKind {
        match self {
            InputContext::Browse => ContextKind::Browse,
            InputContext::Help { .. } => ContextKind::Help,
            InputContext::ContextMenu(_) => ContextKind::ContextMenu,
            InputContext::Rename(_) => ContextKind::Rename,
            InputContext::DeleteConfirm(_) => ContextKind::DeleteConfirm,
            InputContext::CancelConfirm => ContextKind::CancelConfirm,
            InputContext::CollisionWarning { .. } => ContextKind::CollisionWarning,
            InputContext::OverwriteConfirm { .. } => ContextKind::OverwriteConfirm,
        }
    }
}

/// Stack of input contexts. `Browse` is the permanent bottom. Stack order
/// defines both input priority and modal z-order.
pub struct ContextStack {
    stack: Vec<InputContext>,
}

impl ContextStack {
    pub fn new() -> Self {
        Self {
            stack: vec![InputContext::Browse],
        }
    }

    pub fn top(&self) -> &InputContext {
        self.stack.last().expect("Browse is never popped")
    }

    pub fn push(&mut self, ctx: InputContext) {
        self.stack.push(ctx);
    }

    /// Pop the top context; `Browse` stays.
    pub fn pop(&mut self) -> Option<InputContext> {
        if self.stack.len() > 1 {
            self.stack.pop()
        } else {
            None
        }
    }

    /// Pop the top context only if it is of the given kind.
    pub fn pop_kind(&mut self, kind: ContextKind) -> Option<InputContext> {
        if self.top().kind() == kind {
            self.pop()
        } else {
            None
        }
    }

    pub fn iter(&self) -> std::slice::Iter<'_, InputContext> {
        self.stack.iter()
    }

    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, InputContext> {
        self.stack.iter_mut()
    }
}

// === Event handling ===
//
// The top of the context stack owns input. Each context has one handler per
// event type; context-local mutations (cursor movement, text editing, menu
// navigation) happen directly, while semantic operations are returned as an
// `Action` for the caller to dispatch.

pub fn handle_event(app: &mut App, event: Event) -> Option<Action> {
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => handle_key(app, key),
        Event::Mouse(mouse) => handle_mouse(app, mouse),
        _ => None,
    }
}

fn handle_key(app: &mut App, key: KeyEvent) -> Option<Action> {
    match app.context_kind() {
        ContextKind::Browse => app.keymap.lookup(key),
        ContextKind::Help => help_key(app, key),
        ContextKind::ContextMenu => context_menu_key(app, key),
        ContextKind::Rename => rename_key(app, key),
        ContextKind::DeleteConfirm => delete_confirm_key(key),
        ContextKind::CancelConfirm => cancel_confirm_key(app, key),
        ContextKind::CollisionWarning => collision_warning_key(app, key),
        ContextKind::OverwriteConfirm => overwrite_confirm_key(app, key),
    }
}

fn handle_mouse(app: &mut App, mouse: MouseEvent) -> Option<Action> {
    match app.context_kind() {
        ContextKind::Browse => browse_mouse(app, mouse),
        ContextKind::ContextMenu => context_menu_mouse(app, mouse),
        ContextKind::Help => help_mouse(app, mouse),
        // Modal dialogs ignore mouse input
        _ => None,
    }
}

fn browse_mouse(app: &mut App, mouse: MouseEvent) -> Option<Action> {
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => app.handle_click(mouse.column, mouse.row),
        MouseEventKind::Down(MouseButton::Right) | MouseEventKind::Down(MouseButton::Middle) => {
            app.open_context_menu(mouse.column, mouse.row);
        }
        MouseEventKind::ScrollUp => return Some(Action::MoveUp),
        MouseEventKind::ScrollDown => return Some(Action::MoveDown),
        _ => {}
    }
    None
}

fn help_key(app: &mut App, key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') => {
            return Some(Action::ToggleHelp)
        }
        KeyCode::Up | KeyCode::Char('k') => app.help_scroll(-1),
        KeyCode::Down | KeyCode::Char('j') => app.help_scroll(1),
        KeyCode::PageUp => app.help_scroll_page(-1),
        KeyCode::PageDown => app.help_scroll_page(1),
        KeyCode::Home | KeyCode::Char('g') => app.help_scroll(i32::MIN),
        KeyCode::End | KeyCode::Char('G') => app.help_scroll(i32::MAX),
        _ => {}
    }
    None
}

fn help_mouse(app: &mut App, mouse: MouseEvent) -> Option<Action> {
    match mouse.kind {
        MouseEventKind::ScrollUp => app.help_scroll(-1),
        MouseEventKind::ScrollDown => app.help_scroll(1),
        _ => {}
    }
    None
}

fn context_menu_key(app: &mut App, key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Esc => app.close_context_menu(),
        KeyCode::Up | KeyCode::Char('k') => app.context_menu_up(),
        KeyCode::Down | KeyCode::Char('j') => app.context_menu_down(),
        KeyCode::Enter => return app.context_menu_execute(),
        _ => {}
    }
    None
}

fn context_menu_mouse(app: &mut App, mouse: MouseEvent) -> Option<Action> {
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left) => {
            return app.context_menu_click(mouse.column, mouse.row);
        }
        MouseEventKind::Down(MouseButton::Right) | MouseEventKind::Down(MouseButton::Middle) => {
            // Right-click closes current menu and opens new one at cursor
            app.close_context_menu();
            app.open_context_menu(mouse.column, mouse.row);
        }
        MouseEventKind::ScrollUp => app.context_menu_up(),
        MouseEventKind::ScrollDown => app.context_menu_down(),
        MouseEventKind::Moved => {
            // Highlight item under cursor
            app.context_menu_hover(mouse.column, mouse.row);
        }
        // Ignore release, drag - keep menu open
        _ => {}
    }
    None
}

fn rename_key(app: &mut App, key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Esc => return Some(Action::CancelRename),
        KeyCode::Enter => return Some(Action::ConfirmRename),
        KeyCode::Backspace => app.rename_backspace(),
        KeyCode::Delete => app.rename_delete(),
        KeyCode::Left => app.rename_cursor_left(),
        KeyCode::Right => app.rename_cursor_right(),
        KeyCode::Home => app.rename_cursor_home(),
        KeyCode::End => app.rename_cursor_end(),
        KeyCode::Char(c) => app.rename_input(c),
        _ => {}
    }
    None
}

fn delete_confirm_key(key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') => Some(Action::ConfirmDelete),
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => Some(Action::CancelDelete),
        _ => None,
    }
}

fn cancel_confirm_key(app: &mut App, key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            app.cancel_transfer();
            app.should_quit = true;
        }
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.cancel_transfer();
            app.should_quit = true;
        }
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
            app.input.pop_kind(ContextKind::CancelConfirm);
        }
        _ => {}
    }
    None
}

fn collision_warning_key(app: &mut App, key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Esc | KeyCode::Enter => {
            app.input.pop_kind(ContextKind::CollisionWarning);
        }
        _ => {}
    }
    None
}

fn overwrite_confirm_key(app: &mut App, key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') => app.confirm_overwrite(),
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => app.cancel_overwrite(),
        _ => {}
    }
    None
}
