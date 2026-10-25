//! The file tree: a read-only view of the workspace's folders in the right
//! pane, for finding out what is where before the names to type into the picker
//! are known. Folders are listed one at a time as they are opened, so a large
//! repository costs only what is looked at. Files open with a click; anything
//! that changes the disk is left to the shell.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use super::layout::RightPane;
use super::{Editor, Effect, Focus, Side};

/// One entry of a listed folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeEntry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
}

/// The tree's state, kept while the pane is open and dropped with it — each
/// opening starts again from the file being edited.
#[derive(Debug)]
pub(super) struct TreePane {
    root: PathBuf,
    /// What each folder held when it was last listed.
    listings: HashMap<PathBuf, Vec<TreeEntry>>,
    /// Folders shown open. The root always is.
    expanded: HashSet<PathBuf>,
    /// First visible row.
    scroll: usize,
    /// The file to bring into view once its folder has been listed. Cleared
    /// after that, so a later listing does not yank the view away from wherever
    /// it has been scrolled to since.
    reveal: Option<PathBuf>,
}

impl TreePane {
    /// The visible rows, each an entry at its depth: a folder's children follow
    /// it while it is open and its listing has arrived.
    fn rows(&self) -> Vec<(usize, &TreeEntry)> {
        let mut rows = Vec::new();
        self.push_rows(&self.root, 0, &mut rows);
        rows
    }

    fn push_rows<'a>(
        &'a self,
        folder: &Path,
        depth: usize,
        rows: &mut Vec<(usize, &'a TreeEntry)>,
    ) {
        for entry in self.listings.get(folder).into_iter().flatten() {
            rows.push((depth, entry));
            if entry.is_dir && self.expanded.contains(&entry.path) {
                self.push_rows(&entry.path, depth + 1, rows);
            }
        }
    }
}

/// A row as drawn.
pub struct TreeRowView<'a> {
    pub depth: usize,
    pub entry: &'a TreeEntry,
    pub expanded: bool,
    /// The file the left pane is showing.
    pub current: bool,
}

pub struct TreeView<'a> {
    pub root: String,
    pub rows: Vec<TreeRowView<'a>>,
    pub scroll: usize,
}

impl Editor {
    /// Ctrl+B: show the tree in the right pane, opened down to the file being
    /// edited, or put it away again.
    pub(super) fn toggle_tree(&mut self) -> Vec<Effect> {
        if self.layout.tree().is_some() {
            let restored = self.stashed_split.take().map(RightPane::Editor);
            self.set_right_pane(restored);
            return Vec::new();
        }
        let root = self.workspace_root.clone();
        let current = self.left_document_path();
        let mut expanded = HashSet::from([root.clone()]);
        if let Some(relative) = current
            .as_deref()
            .and_then(|path| path.strip_prefix(&root).ok())
        {
            let mut folder = root.clone();
            for part in relative.parent().into_iter().flat_map(Path::components) {
                folder.push(part);
                expanded.insert(folder.clone());
            }
        }
        let effects = expanded
            .iter()
            .map(|folder| self.list_folder(folder.clone()))
            .collect();
        self.set_right_pane(Some(RightPane::Tree(Box::new(TreePane {
            root,
            listings: HashMap::new(),
            expanded,
            scroll: 0,
            reveal: current,
        }))));
        // The tree is read with the mouse; the keyboard goes on editing.
        self.focus = Focus::Editor(Side::Left);
        effects
    }

    fn list_folder(&self, path: PathBuf) -> Effect {
        Effect::ListDirectory {
            path,
            respect_ignore_files: self.respect_ignore_files(),
        }
    }

    fn left_document_path(&self) -> Option<PathBuf> {
        self.documents
            .get(&self.layout.left.view.doc)
            .and_then(|document| document.path.clone())
    }

    /// Rows the tree can show: everything below its heading line.
    fn tree_visible_rows(&self) -> usize {
        usize::from(self.terminal_size.1.saturating_sub(2)).max(1)
    }

    pub(super) fn apply_directory_listing(
        &mut self,
        path: PathBuf,
        result: Result<Vec<TreeEntry>, String>,
    ) {
        let visible = self.tree_visible_rows();
        let Some(tree) = self.layout.tree_mut() else {
            return;
        };
        let error = match result {
            Ok(entries) => {
                tree.listings.insert(path, entries);
                None
            }
            Err(error) => Some(error),
        };
        if let Some(target) = tree.reveal.clone()
            && let Some(row) = tree
                .rows()
                .iter()
                .position(|(_, entry)| entry.path == target)
        {
            // A third of the way down, so the folders around it are in view.
            tree.scroll = row.saturating_sub(visible / 3);
            tree.reveal = None;
        }
        if let Some(error) = error {
            self.set_status(error);
        }
        self.dirty = true;
    }

    /// A click on row `row` of the tree, counted from its first entry: a folder
    /// opens or closes, a file opens in the left pane.
    pub(super) fn tree_click(&mut self, row: usize) -> Vec<Effect> {
        let Some(tree) = self.layout.tree_mut() else {
            return Vec::new();
        };
        let Some(entry) = tree.rows().get(row).map(|(_, entry)| (*entry).clone()) else {
            return Vec::new();
        };
        self.dirty = true;
        if entry.is_dir {
            if tree.expanded.remove(&entry.path) {
                return Vec::new();
            }
            tree.expanded.insert(entry.path.clone());
            // Listed afresh on every opening, so a folder shows what is in it
            // now rather than when the tree first saw it.
            return vec![self.list_folder(entry.path)];
        }
        self.focus = Focus::Editor(Side::Left);
        self.record_jump_origin();
        self.open_paths([entry.path])
    }

    pub(super) fn scroll_tree(&mut self, delta: isize) {
        let visible = self.tree_visible_rows();
        if let Some(tree) = self.layout.tree_mut() {
            let max = tree.rows().len().saturating_sub(visible);
            tree.scroll = if delta < 0 {
                tree.scroll.saturating_sub(delta.unsigned_abs())
            } else {
                (tree.scroll + delta as usize).min(max)
            };
            self.dirty = true;
        }
    }

    pub fn tree_view(&self) -> Option<TreeView<'_>> {
        let tree = self.layout.tree()?;
        let current = self.left_document_path();
        Some(TreeView {
            root: tree.root.display().to_string(),
            rows: tree
                .rows()
                .into_iter()
                .map(|(depth, entry)| TreeRowView {
                    depth,
                    entry,
                    expanded: entry.is_dir && tree.expanded.contains(&entry.path),
                    current: current.as_ref() == Some(&entry.path),
                })
                .collect(),
            scroll: tree.scroll,
        })
    }
}
