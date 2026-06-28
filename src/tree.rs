//! File tree for the left sidebar (Explorer).

use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct TreeNode {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub depth: usize,
    pub expanded: bool,
    pub children_loaded: bool,
}

pub struct FileTree {
    pub root: PathBuf,
    /// Flat visible list (respects expand/collapse).
    pub entries: Vec<TreeNode>,
    pub selected: usize,
    pub scroll: usize,
}

impl FileTree {
    pub fn new(root: PathBuf) -> Self {
        let mut tree = Self {
            root: root.clone(),
            entries: Vec::new(),
            selected: 0,
            scroll: 0,
        };
        // root entry
        let name = root
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.display().to_string());
        tree.entries.push(TreeNode {
            path: root,
            name,
            is_dir: true,
            depth: 0,
            expanded: true,
            children_loaded: false,
        });
        tree.load_children(0);
        tree
    }

    pub fn refresh(&mut self) {
        let root = self.root.clone();
        let sel_path = self.entries.get(self.selected).map(|e| e.path.clone());
        *self = Self::new(root);
        if let Some(p) = sel_path {
            if let Some(i) = self.entries.iter().position(|e| e.path == p) {
                self.selected = i;
            }
        }
    }

    fn load_children(&mut self, parent_idx: usize) {
        if parent_idx >= self.entries.len() {
            return;
        }
        if self.entries[parent_idx].children_loaded {
            return;
        }
        let parent_path = self.entries[parent_idx].path.clone();
        let depth = self.entries[parent_idx].depth + 1;
        self.entries[parent_idx].children_loaded = true;
        self.entries[parent_idx].expanded = true;

        let mut children: Vec<TreeNode> = Vec::new();
        if let Ok(rd) = fs::read_dir(&parent_path) {
            let mut items: Vec<_> = rd.filter_map(|e| e.ok()).collect();
            items.sort_by(|a, b| {
                let ad = a.file_type().map(|t| t.is_dir()).unwrap_or(false);
                let bd = b.file_type().map(|t| t.is_dir()).unwrap_or(false);
                match (ad, bd) {
                    (true, false) => std::cmp::Ordering::Less,
                    (false, true) => std::cmp::Ordering::Greater,
                    _ => a.file_name().cmp(&b.file_name()),
                }
            });
            for entry in items {
                let path = entry.path();
                // skip hidden by default except . for root listing we skip dotfiles
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') {
                    continue;
                }
                let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                children.push(TreeNode {
                    path,
                    name,
                    is_dir,
                    depth,
                    expanded: false,
                    children_loaded: false,
                });
            }
        }
        // insert after parent
        let insert_at = parent_idx + 1;
        for (i, child) in children.into_iter().enumerate() {
            self.entries.insert(insert_at + i, child);
        }
    }

    fn unload_children(&mut self, parent_idx: usize) {
        if parent_idx >= self.entries.len() {
            return;
        }
        let depth = self.entries[parent_idx].depth;
        self.entries[parent_idx].expanded = false;
        let i = parent_idx + 1;
        while i < self.entries.len() && self.entries[i].depth > depth {
            self.entries.remove(i);
            // do not increment i — next element shifted into place
        }
        self.entries[parent_idx].children_loaded = false;
    }

    pub fn toggle(&mut self) {
        if self.selected >= self.entries.len() {
            return;
        }
        if !self.entries[self.selected].is_dir {
            return;
        }
        if self.entries[self.selected].expanded {
            self.unload_children(self.selected);
        } else {
            self.load_children(self.selected);
        }
    }

    pub fn select_next(&mut self) {
        if self.selected + 1 < self.entries.len() {
            self.selected += 1;
        }
    }

    pub fn select_prev(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    pub fn ensure_visible(&mut self, view_rows: usize) {
        if self.entries.is_empty() {
            return;
        }
        if self.selected >= self.entries.len() {
            self.selected = self.entries.len() - 1;
        }
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + view_rows {
            self.scroll = self.selected + 1 - view_rows;
        }
    }

    pub fn selected_path(&self) -> Option<&Path> {
        self.entries.get(self.selected).map(|e| e.path.as_path())
    }

    pub fn selected_is_dir(&self) -> bool {
        self.entries
            .get(self.selected)
            .map(|e| e.is_dir)
            .unwrap_or(false)
    }
}
