use std::collections::HashSet;
use std::ops::ControlFlow;
use std::path::PathBuf;

/// Arena-based file tree with lazy loading and hierarchical selection
pub struct FileTree {
    /// Flat storage of all loaded nodes
    nodes: Vec<TreeNode>,
    /// Indices of root-level nodes
    roots: Vec<usize>,
    /// Root directory path
    pub root_path: String,
    /// Whether this is a remote tree (affects path joining)
    pub is_remote: bool,
    /// Current cursor position (visible index)
    pub cursor: usize,
    /// Explicitly selected node indices
    selected: HashSet<usize>,
    /// Explicitly deselected (overrides ancestor selection)
    deselected: HashSet<usize>,
}

/// A node in the file tree
pub struct TreeNode {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub expanded: bool,
    pub children_loaded: bool,
    /// Parent index in arena (None for root-level nodes)
    pub parent: Option<usize>,
    /// Children indices in arena
    pub children: Vec<usize>,
}

impl TreeNode {
    fn new(name: String, is_dir: bool, size: u64, parent: Option<usize>) -> Self {
        Self {
            name,
            is_dir,
            size,
            expanded: false,
            children_loaded: false,
            parent,
            children: Vec::new(),
        }
    }
}

/// A materialized node view for operations (transfers, rename, delete):
/// identity plus computed paths. Rendering uses the borrowed `RowView`
/// instead, so whole-tree materialization never happens per frame.
#[derive(Debug, Clone)]
pub struct VisibleNode {
    /// Arena index (node identity). Read by tests; operations address
    /// nodes by path.
    #[cfg_attr(not(test), allow(dead_code))]
    pub index: usize,
    pub name: String,
    pub full_path: String,
    pub relative_path: String,  // Path relative to tree root (for structure-preserving transfers)
    pub is_dir: bool,
}

/// Selection state for rendering
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionState {
    None,
    Selected,
    Inherited,
    Partial,
}

/// Per-row view of a visible node: everything the tree renderer needs,
/// borrowed from the arena - no path materialization.
pub struct RowView<'a> {
    pub name: &'a str,
    pub is_dir: bool,
    pub expanded: bool,
    pub size: u64,
    pub depth: usize,
    pub selected: SelectionState,
}

impl FileTree {
    pub fn new(root_path: String, is_remote: bool) -> Self {
        Self {
            nodes: Vec::new(),
            roots: Vec::new(),
            root_path,
            is_remote,
            cursor: 0,
            selected: HashSet::new(),
            deselected: HashSet::new(),
        }
    }

    /// Join path components using appropriate separator
    fn join_path(&self, base: &str, name: &str) -> String {
        if self.is_remote {
            // Remote: always forward slash
            format!("{}/{}", base.trim_end_matches('/'), name)
        } else {
            // Local: platform-aware via PathBuf
            let mut path = PathBuf::from(base);
            path.push(name);
            path.to_string_lossy().to_string()
        }
    }

    /// Get full path for a node by walking up parent chain
    pub fn full_path(&self, index: usize) -> String {
        let mut parts: Vec<&str> = Vec::new();
        let mut idx = Some(index);
        
        while let Some(i) = idx {
            parts.push(&self.nodes[i].name);
            idx = self.nodes[i].parent;
        }
        
        parts.reverse();
        
        // Build path from root
        let mut path = self.root_path.clone();
        for part in parts {
            path = self.join_path(&path, part);
        }
        path
    }

    /// Get relative path from tree root (just the node path without root_path prefix)
    pub fn relative_path(&self, index: usize) -> String {
        let mut parts: Vec<&str> = Vec::new();
        let mut idx = Some(index);
        
        while let Some(i) = idx {
            parts.push(&self.nodes[i].name);
            idx = self.nodes[i].parent;
        }
        
        parts.reverse();
        
        // Join parts with appropriate separator
        if self.is_remote {
            parts.join("/")
        } else {
            let path: PathBuf = parts.iter().collect();
            path.to_string_lossy().to_string()
        }
    }

    /// Set root-level entries (clears existing tree)
    pub fn set_entries(&mut self, entries: Vec<(String, bool, u64)>) {
        self.nodes.clear();
        self.roots.clear();
        self.selected.clear();
        self.deselected.clear();
        self.cursor = 0;

        // Sort: directories first, then alphabetically
        let mut entries = entries;
        entries.sort_by(|a, b| {
            match (a.1, b.1) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => a.0.to_lowercase().cmp(&b.0.to_lowercase()),
            }
        });

        for (name, is_dir, size) in entries {
            let idx = self.nodes.len();
            self.nodes.push(TreeNode::new(name, is_dir, size, None));
            self.roots.push(idx);
        }
    }

    /// Set children for a node (lazy loading)
    pub fn set_children(&mut self, parent_index: usize, entries: Vec<(String, bool, u64)>) {
        // Sort entries
        let mut entries = entries;
        entries.sort_by(|a, b| {
            match (a.1, b.1) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => a.0.to_lowercase().cmp(&b.0.to_lowercase()),
            }
        });

        let mut child_indices = Vec::new();
        for (name, is_dir, size) in entries {
            let idx = self.nodes.len();
            self.nodes.push(TreeNode::new(name, is_dir, size, Some(parent_index)));
            child_indices.push(idx);
        }

        self.nodes[parent_index].children = child_indices;
        self.nodes[parent_index].children_loaded = true;
    }

    /// Depth-first walk over the visible nodes in display order - the
    /// single source of truth for visibility. `visit` receives (arena
    /// index, depth) and may stop the walk early; nothing is allocated or
    /// materialized per node, which keeps prefix queries (count, cursor
    /// resolution, the render window) cheap on huge expanded trees.
    pub fn walk_visible(&self, visit: &mut dyn FnMut(usize, usize) -> ControlFlow<()>) {
        fn recurse(
            tree: &FileTree,
            index: usize,
            depth: usize,
            visit: &mut dyn FnMut(usize, usize) -> ControlFlow<()>,
        ) -> ControlFlow<()> {
            visit(index, depth)?;
            let node = &tree.nodes[index];
            if node.expanded {
                for &child_idx in &node.children {
                    recurse(tree, child_idx, depth + 1, visit)?;
                }
            }
            ControlFlow::Continue(())
        }

        for &root_idx in &self.roots {
            if recurse(self, root_idx, 0, visit).is_break() {
                return;
            }
        }
    }

    /// Arena index and depth of the nth visible row.
    fn visible_nth(&self, n: usize) -> Option<(usize, usize)> {
        let mut pos = 0;
        let mut found = None;
        self.walk_visible(&mut |index, depth| {
            if pos == n {
                found = Some((index, depth));
                return ControlFlow::Break(());
            }
            pos += 1;
            ControlFlow::Continue(())
        });
        found
    }

    /// Materialize the full view of one node, paths included.
    fn make_visible_node(&self, index: usize) -> VisibleNode {
        let node = &self.nodes[index];
        VisibleNode {
            index,
            name: node.name.clone(),
            full_path: self.full_path(index),
            relative_path: self.relative_path(index),
            is_dir: node.is_dir,
        }
    }

    /// Borrowed per-row data for the renderer.
    pub fn row_view(&self, index: usize, depth: usize) -> RowView<'_> {
        let node = &self.nodes[index];
        RowView {
            name: &node.name,
            is_dir: node.is_dir,
            expanded: node.expanded,
            size: node.size,
            depth,
            selected: self.visual_selection_state(index),
        }
    }

    /// All visible nodes, fully materialized. Test-only: production paths
    /// go through `walk_visible` so huge trees never pay for whole-tree
    /// path construction.
    #[cfg(test)]
    pub fn visible_nodes(&self) -> Vec<VisibleNode> {
        let mut result = Vec::new();
        self.walk_visible(&mut |index, _depth| {
            result.push(self.make_visible_node(index));
            ControlFlow::Continue(())
        });
        result
    }

    /// Count visible nodes (no materialization).
    pub fn visible_count(&self) -> usize {
        let mut count = 0;
        self.walk_visible(&mut |_, _| {
            count += 1;
            ControlFlow::Continue(())
        });
        count
    }

    /// Get node at cursor position
    pub fn get_cursor_node(&self) -> Option<VisibleNode> {
        self.visible_nth(self.cursor)
            .map(|(index, _depth)| self.make_visible_node(index))
    }

    /// Get arena index for visible index
    fn visible_to_arena(&self, visible_index: usize) -> Option<usize> {
        self.visible_nth(visible_index).map(|(index, _)| index)
    }

    // === Refresh state capture/restore ===

    /// Full paths of every visibly expanded directory, parents before
    /// children (display order) - the state a refresh must rebuild.
    /// Expansion remembered under a collapsed ancestor is not included:
    /// it is unreachable without re-expanding the ancestor, which reloads.
    pub fn expanded_dirs(&self) -> Vec<String> {
        let mut dirs = Vec::new();
        self.walk_visible(&mut |index, _| {
            let node = &self.nodes[index];
            if node.is_dir && node.expanded {
                dirs.push(self.full_path(index));
            }
            ControlFlow::Continue(())
        });
        dirs
    }

    /// Arena index of the visible directory with this full path.
    pub fn find_visible_dir(&self, path: &str) -> Option<usize> {
        let mut found = None;
        self.walk_visible(&mut |index, _| {
            if self.nodes[index].is_dir && self.full_path(index) == path {
                found = Some(index);
                return ControlFlow::Break(());
            }
            ControlFlow::Continue(())
        });
        found
    }

    /// Attach children to a node and mark it expanded (refresh restore).
    pub fn expand_with_children(&mut self, index: usize, entries: Vec<(String, bool, u64)>) {
        self.set_children(index, entries);
        self.nodes[index].expanded = true;
    }

    /// Full path of the node under the cursor, if any.
    pub fn cursor_path(&self) -> Option<String> {
        self.visible_to_arena(self.cursor).map(|index| self.full_path(index))
    }

    /// Put the cursor back on the row with this full path. Returns false
    /// (cursor untouched) when the path is no longer visible.
    pub fn cursor_to_path(&mut self, path: &str) -> bool {
        let mut pos = 0;
        let mut found = None;
        self.walk_visible(&mut |index, _| {
            if self.full_path(index) == path {
                found = Some(pos);
                return ControlFlow::Break(());
            }
            pos += 1;
            ControlFlow::Continue(())
        });
        match found {
            Some(pos) => {
                self.cursor = pos;
                true
            }
            None => false,
        }
    }

    // === Navigation ===

    pub fn move_up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn move_down(&mut self) {
        let count = self.visible_count();
        if count > 0 && self.cursor + 1 < count {
            self.cursor += 1;
        }
    }

    /// Move the cursor by a signed number of rows, clamped to the visible
    /// range (page-wise navigation).
    pub fn move_by(&mut self, delta: isize) {
        if delta < 0 {
            self.cursor = self.cursor.saturating_sub(delta.unsigned_abs());
        } else {
            let count = self.visible_count();
            if count > 0 {
                self.cursor = (self.cursor + delta as usize).min(count - 1);
            }
        }
    }

    pub fn go_to_top(&mut self) {
        self.cursor = 0;
    }

    pub fn go_to_bottom(&mut self) {
        let count = self.visible_count();
        if count > 0 {
            self.cursor = count - 1;
        }
    }

    pub fn clamp_cursor(&mut self) {
        let count = self.visible_count();
        if count == 0 {
            self.cursor = 0;
        } else if self.cursor >= count {
            self.cursor = count - 1;
        }
    }

    // === Expansion ===

    /// Toggle expand/collapse. Returns path if children need loading.
    pub fn toggle_expand(&mut self) -> Option<(usize, String)> {
        let arena_idx = self.visible_to_arena(self.cursor)?;
        
        // Copy needed values to avoid borrow conflicts
        let is_dir = self.nodes[arena_idx].is_dir;
        let was_expanded = self.nodes[arena_idx].expanded;
        let children_loaded = self.nodes[arena_idx].children_loaded;

        if !is_dir {
            return None;
        }

        if was_expanded {
            self.nodes[arena_idx].expanded = false;
            None
        } else {
            self.nodes[arena_idx].expanded = true;
            if !children_loaded {
                Some((arena_idx, self.full_path(arena_idx)))
            } else {
                None
            }
        }
    }

    /// Collapse node at cursor
    pub fn collapse(&mut self) -> bool {
        if let Some(arena_idx) = self.visible_to_arena(self.cursor) {
            if self.nodes[arena_idx].expanded {
                self.nodes[arena_idx].expanded = false;
                return true;
            }
        }
        false
    }

    // === Selection ===
    //
    // Selection is a recursive property of the tree. Two explicit marker
    // sets (`selected`, `deselected`) are the ground truth; everything else
    // is derived recursively:
    //
    // - Downward: a node inherits selection from its nearest explicitly
    //   marked ancestor (`has_selected_ancestor`, nearest marker wins).
    // - Upward: a directory with loaded children derives its state from
    //   them (`visual_selection_state`).
    // - Operations: `walk_selected` recursively visits the top-level
    //   selected subtree roots; transfer collection, counting, and menu
    //   labels all consume this single traversal, so what is rendered and
    //   what is operated on cannot diverge.

    /// Compute visual selection state recursively based on children
    /// This is what gets rendered - separates display from controlled state
    pub fn visual_selection_state(&self, index: usize) -> SelectionState {
        let node = &self.nodes[index];

        // If explicitly deselected, check if any descendants are selected
        if self.deselected.contains(&index) {
            if self.has_selected_descendant(index) {
                return SelectionState::Partial;
            }
            return SelectionState::None;
        }

        // If explicitly selected
        if self.selected.contains(&index) {
            if self.has_deselected_descendant(index) {
                return SelectionState::Partial;
            }
            return SelectionState::Selected;
        }

        // Not explicitly selected/deselected - compute from children or ancestors
        if node.is_dir && node.children_loaded && !node.children.is_empty() {
            // Compute state from children
            let mut all_selected = true;
            let mut any_selected = false;
            let mut any_partial = false;

            for &child_idx in &node.children {
                match self.visual_selection_state(child_idx) {
                    SelectionState::Selected | SelectionState::Inherited => {
                        any_selected = true;
                    }
                    SelectionState::Partial => {
                        any_selected = true;
                        any_partial = true;
                        all_selected = false;
                    }
                    SelectionState::None => {
                        all_selected = false;
                    }
                }
            }

            if all_selected && any_selected {
                return SelectionState::Selected;
            } else if any_selected || any_partial {
                return SelectionState::Partial;
            } else {
                return SelectionState::None;
            }
        }

        // No children loaded or not a dir - check ancestors
        if self.has_selected_ancestor(index) {
            return SelectionState::Inherited;
        }

        SelectionState::None
    }

    /// Whether selection is inherited from above. Recursive walk where the
    /// nearest explicitly marked ancestor wins: a deselected ancestor blocks
    /// inheritance, a selected one grants it.
    fn has_selected_ancestor(&self, index: usize) -> bool {
        match self.nodes[index].parent {
            None => false,
            Some(parent) => {
                if self.deselected.contains(&parent) {
                    false
                } else if self.selected.contains(&parent) {
                    true
                } else {
                    self.has_selected_ancestor(parent)
                }
            }
        }
    }

    fn has_selected_descendant(&self, index: usize) -> bool {
        self.nodes[index].children.iter().any(|&child_idx| {
            self.selected.contains(&child_idx) || self.has_selected_descendant(child_idx)
        })
    }

    fn has_deselected_descendant(&self, index: usize) -> bool {
        self.nodes[index].children.iter().any(|&child_idx| {
            self.deselected.contains(&child_idx) || self.has_deselected_descendant(child_idx)
        })
    }

    /// Toggle selection at cursor. Ancestor-aware: under a selected ancestor
    /// the node cycles Inherited <-> None via the `deselected` set; explicit
    /// selection is only ever introduced where no ancestor already covers
    /// the node, so nested explicit selections cannot arise.
    pub fn toggle_select(&mut self) {
        let Some(arena_idx) = self.visible_to_arena(self.cursor) else { return };

        match self.visual_selection_state(arena_idx) {
            SelectionState::None => {
                // Include this node: lift a deselection if one exists, and
                // only mark explicitly when no selected ancestor covers it.
                self.deselected.remove(&arena_idx);
                if !self.has_selected_ancestor(arena_idx) {
                    self.selected.insert(arena_idx);
                }
                // Clear any descendant marks (now redundant)
                self.clear_descendant_selection(arena_idx);
            }
            SelectionState::Selected => {
                if self.selected.remove(&arena_idx) {
                    // Was explicitly selected. If an ancestor still covers
                    // this node, removing the mark alone would leave it
                    // Inherited - record a deselection to actually exclude it.
                    if self.has_selected_ancestor(arena_idx) {
                        self.deselected.insert(arena_idx);
                    }
                } else if self.has_selected_ancestor(arena_idx) {
                    // Computed-selected via inherited children under a
                    // selected ancestor - exclude this whole subtree.
                    self.deselected.insert(arena_idx);
                    self.clear_descendant_selection(arena_idx);
                } else {
                    // Computed-selected from explicitly selected children -
                    // deselect them all.
                    self.clear_descendant_selection(arena_idx);
                }
            }
            SelectionState::Inherited => {
                // Under selected ancestor - explicitly exclude
                self.deselected.insert(arena_idx);
            }
            SelectionState::Partial => {
                // Partially selected - complete the selection
                self.deselected.remove(&arena_idx);
                self.clear_descendant_selection(arena_idx);
                if !self.has_selected_ancestor(arena_idx) {
                    self.selected.insert(arena_idx);
                }
            }
        }
    }

    fn clear_descendant_selection(&mut self, index: usize) {
        let children: Vec<usize> = self.nodes[index].children.clone();
        for child_idx in children {
            self.selected.remove(&child_idx);
            self.deselected.remove(&child_idx);
            self.clear_descendant_selection(child_idx);
        }
    }

    /// True when every root renders as fully selected. Shared predicate for
    /// `select_all`'s toggle and the context menu label.
    pub fn all_selected(&self) -> bool {
        !self.roots.is_empty()
            && self.roots.iter().all(|&idx| {
                matches!(self.visual_selection_state(idx), SelectionState::Selected)
            })
    }

    /// Select all roots, or clear everything if already fully selected
    pub fn select_all(&mut self) {
        if self.all_selected() {
            self.clear_selection();
        } else {
            self.selected.clear();
            self.deselected.clear();
            let roots = self.roots.clone();
            self.selected.extend(roots);
        }
    }

    /// Clear all selections
    pub fn clear_selection(&mut self) {
        self.selected.clear();
        self.deselected.clear();
    }

    pub fn has_selection(&self) -> bool {
        !self.selected.is_empty()
    }

    /// Number of top-level selected subtrees. Counts the same units that
    /// `get_selected_for_transfer` returns, independent of expansion state.
    pub fn selection_count(&self) -> usize {
        let (files, dirs) = self.selection_counts();
        files + dirs
    }

    /// Top-level selected units split by kind: (files, directories).
    /// Same units as `get_selected_for_transfer`.
    pub fn selection_counts(&self) -> (usize, usize) {
        let mut files = 0;
        let mut dirs = 0;
        for &root_idx in &self.roots {
            self.walk_selected(root_idx, &mut |index| {
                if self.nodes[index].is_dir {
                    dirs += 1;
                } else {
                    files += 1;
                }
            });
        }
        (files, dirs)
    }

    // === Transfer helpers ===

    /// Recursive traversal visiting the top-level selected subtree roots:
    /// explicitly selected nodes, ancestor-covered (inherited) nodes, and -
    /// for directories whose selection is computed purely from explicitly
    /// selected children - those children individually. Deselected subtrees
    /// are pruned. This is the single source of truth for what selection
    /// operations act on.
    fn walk_selected(&self, index: usize, visit: &mut dyn FnMut(usize)) {
        match self.visual_selection_state(index) {
            SelectionState::Selected => {
                if self.selected.contains(&index) || self.has_selected_ancestor(index) {
                    // Explicit mark or full ancestor coverage: one whole unit.
                    visit(index);
                } else {
                    // Computed from explicitly selected children: act on the
                    // children, not the directory (e.g. deleting them must
                    // not remove the directory itself).
                    for &child_idx in &self.nodes[index].children {
                        self.walk_selected(child_idx, visit);
                    }
                }
            }
            SelectionState::Inherited => visit(index),
            SelectionState::Partial => {
                for &child_idx in &self.nodes[index].children {
                    self.walk_selected(child_idx, visit);
                }
            }
            SelectionState::None => {}
        }
    }

    /// Get selected nodes for transfer, handling partial selections.
    /// Falls back to the cursor node when nothing is selected.
    pub fn get_selected_for_transfer(&self) -> Vec<VisibleNode> {
        if self.selected.is_empty() {
            // Use cursor
            return self.get_cursor_node().into_iter().collect();
        }

        let mut result = Vec::new();
        for &root_idx in &self.roots {
            self.walk_selected(root_idx, &mut |index| {
                result.push(self.make_visible_node(index));
            });
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tree layout (remote semantics, root "/root"):
    ///   A/        (idx 0, children loaded, expanded)
    ///     B/      (idx 2, dir)
    ///     a1.txt  (idx 3)
    ///   top.txt   (idx 1)
    /// B's children are loaded separately by tests that need them.
    fn sample_tree() -> FileTree {
        let mut tree = FileTree::new("/root".to_string(), true);
        tree.set_entries(vec![
            ("A".to_string(), true, 0),
            ("top.txt".to_string(), false, 10),
        ]);
        tree.set_children(0, vec![
            ("B".to_string(), true, 0),
            ("a1.txt".to_string(), false, 5),
        ]);
        tree.nodes[0].expanded = true;
        tree
    }

    fn load_b_children(tree: &mut FileTree) {
        tree.set_children(2, vec![
            ("b1.txt".to_string(), false, 1),
            ("b2.txt".to_string(), false, 2),
        ]);
        tree.nodes[2].expanded = true;
    }

    /// Move the cursor to the visible row of the given arena index.
    fn cursor_to(tree: &mut FileTree, arena_idx: usize) {
        let pos = tree
            .visible_nodes()
            .iter()
            .position(|n| n.index == arena_idx)
            .expect("node not visible");
        tree.cursor = pos;
    }

    fn toggle_at(tree: &mut FileTree, arena_idx: usize) {
        cursor_to(tree, arena_idx);
        tree.toggle_select();
    }

    fn transfer_indices(tree: &FileTree) -> Vec<usize> {
        let mut indices: Vec<usize> = tree
            .get_selected_for_transfer()
            .iter()
            .map(|n| n.index)
            .collect();
        indices.sort();
        indices
    }

    #[test]
    fn entries_sort_dirs_first() {
        let tree = sample_tree();
        assert!(tree.nodes[tree.roots[0]].is_dir);
        assert!(!tree.nodes[tree.roots[1]].is_dir);
    }

    #[test]
    fn full_path_remote_and_local() {
        let tree = sample_tree();
        assert_eq!(tree.full_path(3), "/root/A/a1.txt");

        let mut local = FileTree::new("/home/user".to_string(), false);
        local.set_entries(vec![("docs".to_string(), true, 0)]);
        local.set_children(0, vec![("file.txt".to_string(), false, 100)]);
        let path = local.full_path(1);
        assert!(path.contains("docs") && path.contains("file.txt"));
    }

    #[test]
    fn refresh_state_roundtrip_restores_expansion_and_cursor() {
        // The capture/restore cycle refresh_pane drives: remember expanded
        // dirs and the cursor path, rebuild the tree from fresh listings,
        // re-expand in captured (display) order, put the cursor back.
        let mut tree = sample_tree();
        load_b_children(&mut tree);
        cursor_to(&mut tree, 5); // b2.txt, deep inside A/B

        let expanded = tree.expanded_dirs();
        assert_eq!(expanded, vec!["/root/A", "/root/A/B"], "parents first");
        let cursor_path = tree.cursor_path().unwrap();
        assert_eq!(cursor_path, "/root/A/B/b2.txt");

        // Rebuild as refresh does, with a new file appearing at the root.
        let mut fresh = FileTree::new("/root".to_string(), true);
        fresh.set_entries(vec![
            ("A".to_string(), true, 0),
            ("new.txt".to_string(), false, 3),
            ("top.txt".to_string(), false, 10),
        ]);
        for dir in &expanded {
            let index = fresh.find_visible_dir(dir).expect("dir still exists");
            let children = match dir.as_str() {
                "/root/A" => vec![
                    ("B".to_string(), true, 0),
                    ("a1.txt".to_string(), false, 5),
                ],
                _ => vec![
                    ("b1.txt".to_string(), false, 1),
                    ("b2.txt".to_string(), false, 2),
                ],
            };
            fresh.expand_with_children(index, children);
        }

        assert!(fresh.cursor_to_path(&cursor_path));
        let names: Vec<String> = fresh.visible_nodes().iter().map(|n| n.name.clone()).collect();
        assert_eq!(names, vec!["A", "B", "b1.txt", "b2.txt", "a1.txt", "new.txt", "top.txt"]);
        assert_eq!(fresh.visible_nodes()[fresh.cursor].name, "b2.txt");

        // A vanished directory is simply not found; a vanished cursor path
        // reports false so the caller can fall back to the old row index.
        assert!(fresh.find_visible_dir("/root/gone").is_none());
        assert!(!fresh.cursor_to_path("/root/A/B/deleted.txt"));
        assert_eq!(fresh.visible_nodes()[fresh.cursor].name, "b2.txt", "cursor untouched");
    }

    #[test]
    fn toggle_cycles_at_top_level() {
        let mut tree = sample_tree();
        toggle_at(&mut tree, 1);
        assert_eq!(tree.visual_selection_state(1), SelectionState::Selected);
        assert!(tree.has_selection());

        toggle_at(&mut tree, 1);
        assert_eq!(tree.visual_selection_state(1), SelectionState::None);
        assert!(!tree.has_selection());
        assert!(tree.deselected.is_empty());
    }

    #[test]
    fn inherited_toggle_cycles_without_nesting() {
        let mut tree = sample_tree();
        toggle_at(&mut tree, 0); // select A
        assert_eq!(tree.visual_selection_state(3), SelectionState::Inherited);

        // Toggle a1 off, then on again: must return to Inherited without
        // creating a nested explicit selection (the old double-transfer bug).
        toggle_at(&mut tree, 3);
        assert_eq!(tree.visual_selection_state(3), SelectionState::None);
        toggle_at(&mut tree, 3);
        assert_eq!(tree.visual_selection_state(3), SelectionState::Inherited);

        assert_eq!(tree.selected.len(), 1, "no nested explicit selection");
        assert!(tree.deselected.is_empty());
        assert_eq!(transfer_indices(&tree), vec![0], "A transferred once, whole");
    }

    #[test]
    fn deselected_subtree_children_render_unselected() {
        let mut tree = sample_tree();
        toggle_at(&mut tree, 0); // select A
        toggle_at(&mut tree, 2); // deselect collapsed dir B

        // Expanding B afterwards must not show its children as selected
        // (the old has_selected_ancestor walked past the deselected B to A).
        load_b_children(&mut tree);
        assert_eq!(tree.visual_selection_state(4), SelectionState::None);
        assert_eq!(tree.visual_selection_state(5), SelectionState::None);

        // ...and operations agree: only a1 under A is included.
        assert_eq!(transfer_indices(&tree), vec![3]);
        assert_eq!(tree.selection_count(), 1);
    }

    #[test]
    fn expanded_dir_under_selected_ancestor_can_be_excluded() {
        let mut tree = sample_tree();
        load_b_children(&mut tree);
        toggle_at(&mut tree, 0); // select A; B is computed-Selected via inherited children

        // Toggling B must exclude it (the old code was a silent no-op here).
        toggle_at(&mut tree, 2);
        assert_eq!(tree.visual_selection_state(2), SelectionState::None);
        assert_eq!(tree.visual_selection_state(4), SelectionState::None);
        assert_eq!(transfer_indices(&tree), vec![3]);

        // Toggling again restores full inclusion through the ancestor.
        toggle_at(&mut tree, 2);
        assert_eq!(tree.visual_selection_state(2), SelectionState::Selected);
        assert_eq!(transfer_indices(&tree), vec![0]);
        assert_eq!(tree.selected.len(), 1, "still only A explicitly selected");
    }

    #[test]
    fn computed_selection_acts_on_children_not_directory() {
        let mut tree = sample_tree();
        load_b_children(&mut tree);
        toggle_at(&mut tree, 4); // select b1
        toggle_at(&mut tree, 5); // select b2

        // B renders fully selected, but operations act on the children so
        // e.g. delete does not remove B itself.
        assert_eq!(tree.visual_selection_state(2), SelectionState::Selected);
        assert_eq!(transfer_indices(&tree), vec![4, 5]);
        assert_eq!(tree.selection_count(), 2);

        // Toggling computed-Selected B clears the children.
        toggle_at(&mut tree, 2);
        assert!(!tree.has_selection());
    }

    #[test]
    fn count_matches_transfer_and_ignores_expansion() {
        let mut tree = sample_tree();
        toggle_at(&mut tree, 3); // select a1 inside A

        assert_eq!(tree.selection_count(), transfer_indices(&tree).len());

        // Collapsing A must not change the count (old count was visible-only).
        tree.nodes[0].expanded = false;
        assert_eq!(tree.selection_count(), 1);
        assert_eq!(transfer_indices(&tree), vec![3]);
        assert_eq!(tree.visual_selection_state(0), SelectionState::Partial);
    }

    #[test]
    fn partial_selection_prunes_deselected_only() {
        let mut tree = sample_tree();
        load_b_children(&mut tree);
        toggle_at(&mut tree, 0); // select A
        toggle_at(&mut tree, 3); // deselect a1

        // B stays covered by A as a whole unit; a1 is excluded.
        assert_eq!(transfer_indices(&tree), vec![2]);
        assert_eq!(tree.selection_count(), 1);
        assert_eq!(tree.visual_selection_state(0), SelectionState::Partial);
    }

    #[test]
    fn select_all_toggles_with_shared_predicate() {
        let mut tree = sample_tree();
        assert!(!tree.all_selected());

        tree.select_all();
        assert!(tree.all_selected());
        assert_eq!(tree.selection_count(), 2); // two roots
        assert_eq!(transfer_indices(&tree), vec![0, 1]);

        tree.select_all();
        assert!(!tree.has_selection());
    }

    #[test]
    fn selection_counts_split_files_and_dirs() {
        let mut tree = sample_tree();
        toggle_at(&mut tree, 0); // dir A
        toggle_at(&mut tree, 1); // file top.txt
        assert_eq!(tree.selection_counts(), (1, 1));
        assert_eq!(tree.selection_count(), 2);

        // Excluding a1 splits A into its parts: dir B stays a unit.
        toggle_at(&mut tree, 3);
        assert_eq!(tree.selection_counts(), (1, 1)); // top.txt + B
    }

    #[test]
    fn cursor_fallback_when_nothing_selected() {
        let mut tree = sample_tree();
        cursor_to(&mut tree, 1);
        let nodes = tree.get_selected_for_transfer();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].index, 1);
    }
}
