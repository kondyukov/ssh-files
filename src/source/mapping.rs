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

/// What to do with symbolic links encountered while walking a source.
///
/// A source enumerated over SFTP is attacker-controllable: the server
/// chooses what a `readdir` entry's type and target are. Following a link
/// would let it materialize a server-chosen target (e.g. `/etc/shadow`, or
/// `/dev/zero` for an unbounded read) under an innocuous name, and we have
/// no API to *recreate* a link at the destination. So v0.3 skips them.
///
/// `Follow` is reserved: the walk already records every link as a
/// [`WalkEntry`] with `is_symlink` set, so a future `scp -r`-style mode can
/// be turned on once destination link-creation and loop detection exist.
/// It is not yet wired to any transfer path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SymlinkPolicy {
    /// Do not follow, descend into, or transfer symbolic links. They are
    /// counted ([`CollectedFiles::skipped_symlinks`]) and reported.
    #[default]
    Skip,
    /// Reserved for a future release: follow links, transferring each
    /// link's target as content. Not implemented.
    #[allow(dead_code)]
    Follow,
}

/// A single path component is safe only if it names exactly one entry
/// *inside* its parent directory. Anything else — an empty name, `.`/`..`,
/// or a name carrying a path separator or NUL — is a path-traversal vector
/// (scp CVE-2019-6111 class): a malicious server can hand back a `readdir`
/// entry named `../../.bashrc` or `/etc/cron.d/evil` and, unchecked, we
/// would compose a destination path that escapes the transfer root.
///
/// This is the choke point every executor trusts. The remote walk rejects
/// hostile names before they ever reach a [`WalkEntry`]; this predicate is
/// the shared definition and the defense-in-depth guard inside the mapper.
pub fn is_safe_component(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0')
}

/// One entry discovered while walking a subtree: the absolute path on the
/// source filesystem plus its path components relative to the walk root.
/// The walk root itself appears first with empty `components`.
#[derive(Debug, Clone)]
pub struct WalkEntry {
    pub full_path: String,
    pub components: Vec<String>,
    pub is_dir: bool,
    /// The entry is a symbolic link (not followed; see [`SymlinkPolicy`]).
    /// Mutually exclusive with `is_dir` in practice — a link is recorded by
    /// its own type, never resolved to its target.
    pub is_symlink: bool,
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
        // v0.3 symlink policy: links are neither followed nor recreated.
        // The walk records them so a future SymlinkPolicy::Follow can act
        // here; for now they are counted and skipped.
        if entry.is_symlink {
            collected.skipped_symlinks += 1;
            continue;
        }

        // Defense in depth. The remote walk already refuses hostile names
        // at the wire boundary, but this mapper is the single point every
        // executor trusts, so no future source can smuggle a traversal
        // component past it. In debug this trips loudly; in release the
        // entry is dropped rather than allowed to escape the root.
        if entry.components.iter().any(|c| !is_safe_component(c)) {
            debug_assert!(false, "unsafe path component reached mapper: {:?}", entry.components);
            continue;
        }

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
            is_symlink: false,
            size: 0,
        }
    }

    fn file(full: &str, components: &[&str], size: u64) -> WalkEntry {
        WalkEntry {
            full_path: full.to_string(),
            components: components.iter().map(|s| s.to_string()).collect(),
            is_dir: false,
            is_symlink: false,
            size,
        }
    }

    fn link(full: &str, components: &[&str]) -> WalkEntry {
        WalkEntry {
            full_path: full.to_string(),
            components: components.iter().map(|s| s.to_string()).collect(),
            is_dir: false,
            is_symlink: true,
            size: 0,
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

    #[test]
    fn traversal_components_are_rejected() {
        // Names a hostile server could return from readdir. None of these
        // may become a safe component; the mapper's defense-in-depth guard
        // must drop any that slip past the walk boundary.
        for hostile in ["..", ".", "", "../etc", "a/b", "a\\b", "/etc/passwd", "x\0y"] {
            assert!(!is_safe_component(hostile), "{hostile:?} must be rejected");
        }
        for ok in ["file.txt", "a b", "..hidden", "...", "naïve", "-rf"] {
            assert!(is_safe_component(ok), "{ok:?} must be accepted");
        }
    }

    #[test]
    fn mapper_drops_entry_bearing_traversal_component() {
        // A WalkEntry whose components escape the root must not produce a
        // file mapping (belt-and-suspenders behind the walk-boundary abort).
        // debug_assert would fire in debug, so exercise the release path.
        #[cfg(not(debug_assertions))]
        {
            let mut collected = CollectedFiles::new();
            let entries = vec![WalkEntry {
                full_path: "/src/evil".to_string(),
                components: vec!["..".to_string(), "..".to_string(), ".bashrc".to_string()],
                is_dir: false,
                is_symlink: false,
                size: 9,
            }];
            map_to_destination(true, "D", entries, &mut collected);
            assert!(collected.files.is_empty());
        }
    }

    #[test]
    fn symlinks_are_skipped_and_counted() {
        let mut collected = CollectedFiles::new();
        let entries = vec![
            dir("/src/D", &[]),
            file("/src/D/real.txt", &["real.txt"], 4),
            link("/src/D/evil", &["evil"]),
            link("/src/D/sub/loop", &["sub", "loop"]),
        ];
        map_to_destination(true, "D", entries, &mut collected);

        let rels: Vec<&str> = collected.files.iter().map(|f| f.relative_path.as_str()).collect();
        assert_eq!(rels, vec!["D/real.txt"]);
        assert_eq!(collected.skipped_symlinks, 2);
    }
}
