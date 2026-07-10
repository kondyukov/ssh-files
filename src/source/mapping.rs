//! The single source of truth for source -> destination path mapping.
//!
//! Sources *enumerate*: `FileSource::walk` yields neutral path components
//! relative to the walked root, with no layout opinions. This module
//! *maps*: every `relative_path` and directory the executors will create
//! is decided here, and nowhere else. Executors then apply
//! `dest_base + relative_path` blindly.
//!
//! Both modes are projections of one canonical path,
//! `anchor components + entry components`, where `anchor` is the selected
//! node's path relative to the source pane root (exactly what the user
//! sees in the tree):
//!
//! - preserve (tree): the whole path - the visible tree is mirrored under
//!   the destination root
//! - flat: the path *above* the selection root is dropped - each selected
//!   item lands in the destination root under its own name, a selected
//!   directory keeping its internal structure intact (GUI drag-and-drop
//!   semantics). The two modes differ only in how much of the anchor's
//!   ancestry survives; the contents of a selected directory are never
//!   restructured.

use super::{CollectedFiles, FileEntry};

/// One entry discovered while walking a subtree: the absolute path on the
/// source filesystem plus its path components relative to the walk root.
/// The walk root itself appears first with empty `components`.
#[derive(Debug, Clone)]
pub struct WalkEntry {
    pub full_path: String,
    pub components: Vec<String>,
    pub is_dir: bool,
    pub size: u64,
}

/// Map walked entries onto destination-relative paths.
pub fn map_to_destination(
    preserve: bool,
    anchor: &str,
    entries: Vec<WalkEntry>,
    collected: &mut CollectedFiles,
) {
    // Tree-relative paths may carry either separator; components are the
    // canonical form.
    let mut anchor_components: Vec<&str> = anchor
        .split(['/', '\\'])
        .filter(|part| !part.is_empty())
        .collect();

    // Flat: the selection root itself is the unit - keep only its own name.
    if !preserve && anchor_components.len() > 1 {
        anchor_components = vec![anchor_components[anchor_components.len() - 1]];
    }

    if preserve && anchor_components.len() > 1 {
        // The anchor's parent chain must exist at the destination. (The
        // anchor itself is covered by the walk root entry when it is a
        // directory, and must not be created when it is a file.)
        let mut prefix = String::new();
        for part in &anchor_components[..anchor_components.len() - 1] {
            if prefix.is_empty() {
                prefix = (*part).to_string();
            } else {
                prefix = format!("{}/{}", prefix, part);
            }
            collected.add_dir(prefix.clone());
        }
    }

    for entry in entries {
        let mut full: Vec<&str> = anchor_components.clone();
        full.extend(entry.components.iter().map(|s| s.as_str()));

        if entry.is_dir {
            if !full.is_empty() {
                collected.add_dir(full.join("/"));
            }
        } else {
            if full.is_empty() {
                continue;
            }
            collected.add_file(FileEntry::new(entry.full_path, full.join("/"), entry.size));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(full: &str, components: &[&str]) -> WalkEntry {
        WalkEntry {
            full_path: full.to_string(),
            components: components.iter().map(|s| s.to_string()).collect(),
            is_dir: true,
            size: 0,
        }
    }

    fn file(full: &str, components: &[&str], size: u64) -> WalkEntry {
        WalkEntry {
            full_path: full.to_string(),
            components: components.iter().map(|s| s.to_string()).collect(),
            is_dir: false,
            size,
        }
    }

    /// A walked directory D at tree path a/b/D containing sub/x.txt and
    /// top.txt, plus an empty directory.
    fn sample_walk() -> Vec<WalkEntry> {
        vec![
            dir("/src/a/b/D", &[]),
            dir("/src/a/b/D/sub", &["sub"]),
            file("/src/a/b/D/sub/x.txt", &["sub", "x.txt"], 5),
            file("/src/a/b/D/top.txt", &["top.txt"], 7),
            dir("/src/a/b/D/empty", &["empty"]),
        ]
    }

    #[test]
    fn preserve_mirrors_visible_tree() {
        let mut collected = CollectedFiles::new();
        map_to_destination(true, "a/b/D", sample_walk(), &mut collected);

        let rels: Vec<&str> = collected.files.iter().map(|f| f.relative_path.as_str()).collect();
        assert_eq!(rels, vec!["a/b/D/sub/x.txt", "a/b/D/top.txt"]);

        // Parent chain, the walked root, subdirs, and empty dirs all exist.
        assert_eq!(
            collected.dirs,
            vec!["a", "a/b", "a/b/D", "a/b/D/sub", "a/b/D/empty"]
        );
    }

    #[test]
    fn preserve_single_file_keeps_tree_location() {
        let mut collected = CollectedFiles::new();
        let entries = vec![file("/src/a/b/f.txt", &[], 3)];
        map_to_destination(true, "a/b/f.txt", entries, &mut collected);

        assert_eq!(collected.files[0].relative_path, "a/b/f.txt");
        // The file's own name must not become a directory.
        assert_eq!(collected.dirs, vec!["a", "a/b"]);
    }

    #[test]
    fn flat_strips_ancestry_keeps_structure_inside() {
        let mut collected = CollectedFiles::new();
        map_to_destination(false, "a/b/D", sample_walk(), &mut collected);

        // The selected directory is the unit: `a/b` is dropped, everything
        // inside D - subdirs, empty dirs - arrives intact under D itself.
        let rels: Vec<&str> = collected.files.iter().map(|f| f.relative_path.as_str()).collect();
        assert_eq!(rels, vec!["D/sub/x.txt", "D/top.txt"]);
        assert_eq!(collected.dirs, vec!["D", "D/sub", "D/empty"]);
    }

    #[test]
    fn flat_equals_preserve_for_top_level_selection() {
        // With the selection root already at the pane root, there is no
        // ancestry to strip: the two modes must agree exactly.
        let mut flat = CollectedFiles::new();
        map_to_destination(false, "D", sample_walk(), &mut flat);
        let mut tree = CollectedFiles::new();
        map_to_destination(true, "D", sample_walk(), &mut tree);

        let rels = |c: &CollectedFiles| -> Vec<String> {
            c.files.iter().map(|f| f.relative_path.clone()).collect()
        };
        assert_eq!(rels(&flat), rels(&tree));
        assert_eq!(flat.dirs, tree.dirs);
    }

    #[test]
    fn flat_single_file_uses_its_name() {
        let mut collected = CollectedFiles::new();
        let entries = vec![file("/src/a/b/f.txt", &[], 3)];
        map_to_destination(false, "a/b/f.txt", entries, &mut collected);
        assert_eq!(collected.files[0].relative_path, "f.txt");
    }

    #[test]
    fn windows_separators_normalize() {
        let mut collected = CollectedFiles::new();
        let entries = vec![file("C:\\src\\a\\b\\f.txt", &[], 3)];
        map_to_destination(true, "a\\b\\f.txt", entries, &mut collected);
        assert_eq!(collected.files[0].relative_path, "a/b/f.txt");
        assert_eq!(collected.dirs, vec!["a", "a/b"]);
    }

    #[test]
    fn top_level_anchor_has_no_parent_dirs() {
        let mut collected = CollectedFiles::new();
        map_to_destination(true, "D", sample_walk(), &mut collected);
        assert_eq!(collected.dirs, vec!["D", "D/sub", "D/empty"]);
        assert_eq!(collected.files[0].relative_path, "D/sub/x.txt");
    }
}
