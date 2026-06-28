//! In-memory text buffer with cursor, viewport, highlight, and undo/redo.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::highlight::Highlighter;

const MAX_UNDO: usize = 200;

/// Snapshot of editable state for undo/redo (not scroll).
#[derive(Clone)]
struct Snapshot {
    lines: Vec<String>,
    cursor_row: usize,
    cursor_col: usize,
    dirty: bool,
}

pub struct Buffer {
    pub path: Option<PathBuf>,
    pub lines: Vec<String>,
    pub cursor_row: usize,
    pub cursor_col: usize,
    pub scroll_row: usize,
    pub scroll_col: usize,
    pub dirty: bool,
    pub readonly: bool,
    pub highlight: Highlighter,
    undo_stack: Vec<Snapshot>,
    redo_stack: Vec<Snapshot>,
    /// Skip recording while applying undo/redo restore.
    history_lock: bool,
}

impl Buffer {
    pub fn empty() -> Self {
        Self {
            path: None,
            lines: vec![String::new()],
            cursor_row: 0,
            cursor_col: 0,
            scroll_row: 0,
            scroll_col: 0,
            dirty: false,
            readonly: false,
            highlight: Highlighter::none(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            history_lock: false,
        }
    }

    pub fn from_path(path: &Path) -> io::Result<Self> {
        let data = fs::read_to_string(path).unwrap_or_else(|_| String::new());
        let mut lines: Vec<String> = data.lines().map(|s| s.to_string()).collect();
        if lines.is_empty() {
            lines.push(String::new());
        }
        let readonly = fs::metadata(path)
            .map(|m| m.permissions().readonly())
            .unwrap_or(false);
        let highlight = Highlighter::from_lines(&lines, Some(path));
        Ok(Self {
            path: Some(path.to_path_buf()),
            lines,
            cursor_row: 0,
            cursor_col: 0,
            scroll_row: 0,
            scroll_col: 0,
            dirty: false,
            readonly,
            highlight,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            history_lock: false,
        })
    }

    pub fn title(&self) -> String {
        match &self.path {
            Some(p) => p
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "untitled".into()),
            None => "untitled".into(),
        }
    }

    pub fn lang_label(&self) -> &'static str {
        self.highlight
            .lang
            .map(|l| l.label())
            .unwrap_or("plaintext")
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            lines: self.lines.clone(),
            cursor_row: self.cursor_row,
            cursor_col: self.cursor_col,
            dirty: self.dirty,
        }
    }

    /// Record current state before a user edit (clears redo).
    pub fn push_undo_point(&mut self) {
        if self.history_lock || self.readonly {
            return;
        }
        self.undo_stack.push(self.snapshot());
        if self.undo_stack.len() > MAX_UNDO {
            let excess = self.undo_stack.len() - MAX_UNDO;
            self.undo_stack.drain(0..excess);
        }
        self.redo_stack.clear();
    }

    fn apply_snapshot(&mut self, snap: Snapshot) {
        self.lines = snap.lines;
        self.cursor_row = snap.cursor_row;
        self.cursor_col = snap.cursor_col;
        self.dirty = snap.dirty;
        self.clamp_cursor();
        let path = self.path.clone();
        self.highlight
            .resync_from_lines(&self.lines, path.as_deref());
    }

    /// Undo last edit. Returns true if something was undone.
    pub fn undo(&mut self) -> bool {
        let Some(prev) = self.undo_stack.pop() else {
            return false;
        };
        self.history_lock = true;
        self.redo_stack.push(self.snapshot());
        if self.redo_stack.len() > MAX_UNDO {
            let excess = self.redo_stack.len() - MAX_UNDO;
            self.redo_stack.drain(0..excess);
        }
        self.apply_snapshot(prev);
        self.history_lock = false;
        true
    }

    /// Redo previously undone edit. Returns true if something was redone.
    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo_stack.pop() else {
            return false;
        };
        self.history_lock = true;
        self.undo_stack.push(self.snapshot());
        if self.undo_stack.len() > MAX_UNDO {
            let excess = self.undo_stack.len() - MAX_UNDO;
            self.undo_stack.drain(0..excess);
        }
        self.apply_snapshot(next);
        self.history_lock = false;
        true
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    fn clamp_cursor(&mut self) {
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        if self.cursor_row >= self.lines.len() {
            self.cursor_row = self.lines.len() - 1;
        }
        let len = self.lines[self.cursor_row].chars().count();
        if self.cursor_col > len {
            self.cursor_col = len;
        }
    }

    pub fn ensure_visible(&mut self, view_rows: usize, view_cols: usize) {
        self.clamp_cursor();
        if self.cursor_row < self.scroll_row {
            self.scroll_row = self.cursor_row;
        } else if self.cursor_row >= self.scroll_row + view_rows {
            self.scroll_row = self.cursor_row + 1 - view_rows;
        }
        if self.cursor_col < self.scroll_col {
            self.scroll_col = self.cursor_col;
        } else if self.cursor_col >= self.scroll_col + view_cols {
            self.scroll_col = self.cursor_col + 1 - view_cols;
        }
    }

    pub fn move_left(&mut self) {
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
        } else if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.lines[self.cursor_row].chars().count();
        }
    }

    pub fn move_right(&mut self) {
        let len = self.lines[self.cursor_row].chars().count();
        if self.cursor_col < len {
            self.cursor_col += 1;
        } else if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            self.cursor_col = 0;
        }
    }

    pub fn move_up(&mut self) {
        if self.cursor_row > 0 {
            self.cursor_row -= 1;
            let len = self.lines[self.cursor_row].chars().count();
            if self.cursor_col > len {
                self.cursor_col = len;
            }
        }
    }

    pub fn move_down(&mut self) {
        if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            let len = self.lines[self.cursor_row].chars().count();
            if self.cursor_col > len {
                self.cursor_col = len;
            }
        }
    }

    pub fn move_home(&mut self) {
        self.cursor_col = 0;
    }

    pub fn move_end(&mut self) {
        self.cursor_col = self.lines[self.cursor_row].chars().count();
    }

    pub fn page_up(&mut self, page: usize) {
        self.cursor_row = self.cursor_row.saturating_sub(page);
        self.clamp_cursor();
    }

    pub fn page_down(&mut self, page: usize) {
        self.cursor_row = (self.cursor_row + page).min(self.lines.len().saturating_sub(1));
        self.clamp_cursor();
    }

    fn notify_edit(
        &mut self,
        start_row: usize,
        start_col: usize,
        old_end_row: usize,
        old_end_col: usize,
        new_text: &str,
    ) {
        let lines = self.lines.clone();
        let path = self.path.clone();
        self.highlight.apply_edit(
            start_row,
            start_col,
            old_end_row,
            old_end_col,
            new_text,
            &lines,
        );
        if self.highlight.lang.is_none() {
            if let Some(ref p) = path {
                self.highlight.resync_from_lines(&self.lines, Some(p.as_path()));
            }
        }
    }

    pub fn insert_char(&mut self, ch: char) {
        if self.readonly {
            return;
        }
        self.push_undo_point();
        let row = self.cursor_row;
        let col = self.cursor_col;
        let line = &mut self.lines[row];
        let byte_idx = char_to_byte(line, col);
        let mut tmp = [0u8; 4];
        let s = ch.encode_utf8(&mut tmp);
        line.insert_str(byte_idx, s);
        self.cursor_col += 1;
        self.dirty = true;
        let inserted = s.to_string();
        self.notify_edit(row, col, row, col, &inserted);
    }

    pub fn insert_newline(&mut self) {
        if self.readonly {
            return;
        }
        self.push_undo_point();
        let row = self.cursor_row;
        let col = self.cursor_col;
        let line = &mut self.lines[row];
        let byte_idx = char_to_byte(line, col);
        let rest = line.split_off(byte_idx);
        self.cursor_row += 1;
        self.lines.insert(self.cursor_row, rest);
        self.cursor_col = 0;
        self.dirty = true;
        self.notify_edit(row, col, row, col, "\n");
    }

    pub fn backspace(&mut self) {
        if self.readonly {
            return;
        }
        self.push_undo_point();
        if self.cursor_col > 0 {
            let row = self.cursor_row;
            let col = self.cursor_col;
            let line = &mut self.lines[row];
            let end = char_to_byte(line, col);
            let start = char_to_byte(line, col - 1);
            line.replace_range(start..end, "");
            self.cursor_col -= 1;
            self.dirty = true;
            self.notify_edit(row, col - 1, row, col, "");
        } else if self.cursor_row > 0 {
            let row = self.cursor_row;
            let cur = self.lines.remove(row);
            self.cursor_row -= 1;
            let prev_len = self.lines[self.cursor_row].chars().count();
            self.cursor_col = prev_len;
            self.lines[self.cursor_row].push_str(&cur);
            self.dirty = true;
            self.notify_edit(self.cursor_row, prev_len, row, 0, "");
        } else {
            // nothing to delete — drop empty undo point
            let _ = self.undo_stack.pop();
        }
    }

    pub fn delete(&mut self) {
        if self.readonly {
            return;
        }
        self.push_undo_point();
        let len = self.lines[self.cursor_row].chars().count();
        if self.cursor_col < len {
            let row = self.cursor_row;
            let col = self.cursor_col;
            let line = &mut self.lines[row];
            let start = char_to_byte(line, col);
            let end = char_to_byte(line, col + 1);
            line.replace_range(start..end, "");
            self.dirty = true;
            self.notify_edit(row, col, row, col + 1, "");
        } else if self.cursor_row + 1 < self.lines.len() {
            let row = self.cursor_row;
            let col = self.cursor_col;
            let next = self.lines.remove(row + 1);
            self.lines[row].push_str(&next);
            self.dirty = true;
            self.notify_edit(row, col, row + 1, 0, "");
        } else {
            let _ = self.undo_stack.pop();
        }
    }

    pub fn insert_tab(&mut self) {
        for _ in 0..4 {
            self.insert_char(' ');
        }
    }

    /// Replace range on one line (char cols) — used by completion / replace.
    pub fn replace_range_chars(
        &mut self,
        row: usize,
        start_col: usize,
        end_col: usize,
        text: &str,
    ) {
        if self.readonly || row >= self.lines.len() {
            return;
        }
        self.push_undo_point();
        let line = &mut self.lines[row];
        let chars: Vec<char> = line.chars().collect();
        let start_col = start_col.min(chars.len());
        let end_col = end_col.min(chars.len()).max(start_col);
        let before: String = chars[..start_col].iter().collect();
        let after: String = chars[end_col..].iter().collect();
        self.lines[row] = format!("{before}{text}{after}");
        self.cursor_row = row;
        self.cursor_col = start_col + text.chars().count();
        self.dirty = true;
        let lines = self.lines.clone();
        self.highlight
            .apply_edit(row, start_col, row, end_col, text, &lines);
    }

    pub fn save(&mut self) -> io::Result<()> {
        let path = self.path.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "no file path — open a file from the tree first (untitled buffer)",
            )
        })?;
        if self.readonly {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "file is read-only",
            ));
        }
        let mut out = self.lines.join("\n");
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        } else if out.is_empty() {
            out.push('\n');
        }
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let tmp = parent.join(format!(
            ".{}.tui-code.tmp",
            path.file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "untitled".into())
        ));
        fs::write(&tmp, &out)?;
        if let Err(_e) = fs::rename(&tmp, path) {
            fs::write(path, &out)?;
            let _ = fs::remove_file(&tmp);
        }
        self.dirty = false;
        self.readonly = false;
        Ok(())
    }
}

fn char_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}
