//! Application state, rendering, input (keyboard + mouse), VS Code–like commands.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::complete::{self, CompletionState};
use crate::editor::Buffer;
use crate::term::{Input, Key, MouseAction, MouseButton, MouseEvent, Terminal};
use crate::theme::Theme;
use crate::shell_panel::{key_to_pty_bytes, ShellPanel};
use crate::tree::FileTree;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Sidebar,
    Editor,
    /// Integrated terminal panel has keyboard focus
    Terminal,
}

/// Modal overlays (VS Code–style).
#[derive(Clone, Debug)]
pub enum Overlay {
    None,
    /// Ctrl+P — quick open file
    QuickOpen {
        query: String,
        cursor: usize,
        selected: usize,
        results: Vec<PathBuf>,
    },
    /// Ctrl+F — find in current file
    Find {
        query: String,
        cursor: usize,
        /// (line, char_col) matches
        matches: Vec<(usize, usize)>,
        match_idx: usize,
    },
    /// Ctrl+H — find + replace
    Replace {
        find: String,
        repl: String,
        /// 0 = find field, 1 = replace field
        field: u8,
        cursor: usize,
        matches: Vec<(usize, usize)>,
        match_idx: usize,
    },
}

/// Clickable menu item in the header bar.
#[derive(Clone, Copy)]
struct MenuItem {
    label: &'static str,
    /// Inclusive start column (1-based) filled at draw time
    col_start: u16,
    col_end: u16,
    action: MenuAction,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MenuAction {
    FileSave,
    FileOpen,
    EditFind,
    EditReplace,
    GoQuickOpen,
    ViewExplorer,
    Terminal,
    Help,
    Quit,
}

pub struct App {
    pub tree: FileTree,
    pub buffers: Vec<Buffer>,
    pub active: usize,
    pub focus: Focus,
    pub sidebar_width: u16,
    pub status_msg: String,
    pub quit: bool,
    pub show_help: bool,
    pending_quit_confirm: bool,
    layout: Layout,
    last_tree_click: Option<(usize, Instant)>,
    pub overlay: Overlay,
    /// Cached recursive file list for Ctrl+P
    file_index: Vec<PathBuf>,
    menu_items: Vec<MenuItem>,
    /// Inline autocomplete popup (VS Code–style).
    completion: Option<CompletionState>,
    /// Bottom integrated terminal (multi-tab, real PTY).
    pub shell_panel: ShellPanel,
    /// Hit targets for terminal tab bar / + button (1-based cols).
    term_tab_hits: Vec<(u16, u16, TermHit)>,
}

#[derive(Clone, Copy, Debug)]
enum TermHit {
    Tab(usize),
    Add,
    Close,
}

/// Geometry from the most recent `draw` (terminal cells are 1-based).
#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pub term_w: u16,
    pub term_h: u16,
    pub side_w: u16,
    /// First content row (below menu + tabs)
    pub content_top: u16,
    pub content_rows: u16,
    pub gutter: u16,
    /// First column of editor **text** (1-based)
    pub editor_text_col: u16,
    /// Column of vertical border between sidebar and editor
    pub border_col: u16,
    pub menu_row: u16,
    pub tab_row: u16,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            term_w: 80,
            term_h: 24,
            side_w: 28,
            content_top: 3,
            content_rows: 20,
            gutter: 4,
            editor_text_col: 34,
            border_col: 29,
            menu_row: 1,
            tab_row: 2,
        }
    }
}

impl App {
    pub fn new(root: PathBuf) -> Self {
        let mut app = Self {
            tree: FileTree::new(root.clone()),
            buffers: vec![Buffer::empty()],
            active: 0,
            focus: Focus::Sidebar,
            sidebar_width: 28,
            status_msg: "Ctrl+P open · Ctrl+F find · Ctrl+H replace · Ctrl+S save · Ctrl+C quit"
                .into(),
            quit: false,
            show_help: false,
            pending_quit_confirm: false,
            layout: Layout::default(),
            last_tree_click: None,
            overlay: Overlay::None,
            file_index: Vec::new(),
            menu_items: Vec::new(),
            completion: None,
            shell_panel: ShellPanel::new(root.clone()),
            term_tab_hits: Vec::new(),
        };
        app.rebuild_file_index();
        app
    }

    pub fn poll_shells(&mut self) {
        self.shell_panel.poll_all();
    }

    fn toggle_terminal_panel(&mut self) {
        if !self.shell_panel.visible {
            self.shell_panel.visible = true;
            let cols = self.layout.term_w.saturating_sub(2).max(40);
            let body = self.shell_panel.height.saturating_sub(1).max(4);
            self.shell_panel.ensure_one(cols, body);
            self.shell_panel.resize_all(cols, body);
            self.focus = Focus::Terminal;
            self.shell_panel.focus = true;
            self.completion = None;
            self.status_msg = "Terminal — type in shell · Ctrl+J editor · click + for new tab".into();
        } else if self.focus == Focus::Terminal {
            // hide panel
            self.shell_panel.visible = false;
            self.shell_panel.focus = false;
            self.focus = Focus::Editor;
            self.status_msg = "Terminal panel hidden (Ctrl+J to show)".into();
        } else {
            // show and focus
            self.focus = Focus::Terminal;
            self.shell_panel.focus = true;
            self.completion = None;
            self.status_msg = "Terminal focused".into();
        }
    }

    fn add_terminal_tab(&mut self) {
        let cols = self.layout.term_w.saturating_sub(2).max(40);
        let body = self.shell_panel.height.saturating_sub(1).max(4);
        match self.shell_panel.add_terminal(cols, body) {
            Ok(()) => {
                self.focus = Focus::Terminal;
                self.status_msg = format!(
                    "New terminal ({})",
                    self.shell_panel.sessions.len()
                );
            }
            Err(e) => self.status_msg = format!("Terminal spawn failed: {e}"),
        }
    }

    fn refresh_completion(&mut self) {
        if !matches!(self.overlay, Overlay::None) || self.show_help {
            self.completion = None;
            return;
        }
        if self.focus != Focus::Editor {
            self.completion = None;
            return;
        }
        let row = self.buf().cursor_row;
        let col = self.buf().cursor_col;
        let lines = self.buf().lines.clone();
        // borrow highlight without holding buf mut
        let items = {
            let hl = &self.buffers[self.active].highlight;
            complete::suggest(&lines, row, col, hl)
        };
        self.completion = items;
    }

    fn accept_completion(&mut self) {
        let Some(state) = self.completion.take() else {
            return;
        };
        let Some(item) = state.selected_item().cloned() else {
            return;
        };
        let row = self.buf().cursor_row;
        let end_col = self.buf().cursor_col;
        let start_col = state.start_col;
        if end_col >= start_col && row < self.buf().lines.len() {
            let insert = item.insert.clone();
            self.buf_mut()
                .replace_range_chars(row, start_col, end_col, &insert);
        }
        self.completion = None;
        self.status_msg = format!("Completed `{}` ({})", item.label, item.kind.label());
    }

    fn dismiss_completion(&mut self) {
        self.completion = None;
    }

    pub fn buf(&self) -> &Buffer {
        &self.buffers[self.active]
    }

    pub fn buf_mut(&mut self) -> &mut Buffer {
        &mut self.buffers[self.active]
    }

    fn rebuild_file_index(&mut self) {
        self.file_index.clear();
        let root = self.tree.root.clone();
        walk_files(&root, &root, &mut self.file_index, 0);
        self.file_index.sort();
    }

    fn request_quit(&mut self, force: bool) {
        if force || !self.buffers.iter().any(|b| b.dirty) {
            self.quit = true;
            self.pending_quit_confirm = false;
            return;
        }
        if self.pending_quit_confirm {
            self.quit = true;
            return;
        }
        self.pending_quit_confirm = true;
        self.status_msg =
            "Unsaved changes — Ctrl+C or Ctrl+Q again to quit, Ctrl+S to save".into();
    }

    pub fn open_path(&mut self, path: PathBuf) {
        if path.is_dir() {
            return;
        }
        if let Some(i) = self
            .buffers
            .iter()
            .position(|b| b.path.as_ref() == Some(&path))
        {
            self.active = i;
            self.focus = Focus::Editor;
            self.status_msg = format!("Opened {}", path.display());
            return;
        }
        match Buffer::from_path(&path) {
            Ok(buf) => {
                let title = buf.title();
                let lang = buf.lang_label();
                if self.buffers.len() == 1
                    && self.buffers[0].path.is_none()
                    && !self.buffers[0].dirty
                    && self.buffers[0].lines.len() == 1
                    && self.buffers[0].lines[0].is_empty()
                {
                    self.buffers[0] = buf;
                    self.active = 0;
                } else {
                    self.buffers.push(buf);
                    self.active = self.buffers.len() - 1;
                }
                self.focus = Focus::Editor;
                self.status_msg = format!("Opened {title} ({lang})");
            }
            Err(e) => self.status_msg = format!("Error: {e}"),
        }
    }

    pub fn open_selected(&mut self) {
        if self.tree.selected_is_dir() {
            self.tree.toggle();
            return;
        }
        let Some(path) = self.tree.selected_path().map(|p| p.to_path_buf()) else {
            return;
        };
        self.open_path(path);
    }

    pub fn close_tab(&mut self) {
        if self.buffers.len() <= 1 {
            self.buffers[0] = Buffer::empty();
            self.active = 0;
            self.focus = Focus::Sidebar;
            self.status_msg = "Buffer cleared".into();
            return;
        }
        self.buffers.remove(self.active);
        if self.active >= self.buffers.len() {
            self.active = self.buffers.len() - 1;
        }
    }

    pub fn next_tab(&mut self) {
        if !self.buffers.is_empty() {
            self.active = (self.active + 1) % self.buffers.len();
        }
    }

    pub fn prev_tab(&mut self) {
        if !self.buffers.is_empty() {
            self.active = if self.active == 0 {
                self.buffers.len() - 1
            } else {
                self.active - 1
            };
        }
    }

    fn do_save(&mut self) {
        match self.buf_mut().save() {
            Ok(()) => {
                let path = self
                    .buf()
                    .path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| self.buf().title());
                self.status_msg = format!("Saved {path}");
                self.pending_quit_confirm = false;
            }
            Err(e) => self.status_msg = format!("Save failed: {e}"),
        }
    }

    fn open_quick_open(&mut self) {
        if self.file_index.is_empty() {
            self.rebuild_file_index();
        }
        let results = filter_files(&self.file_index, "");
        self.overlay = Overlay::QuickOpen {
            query: String::new(),
            cursor: 0,
            selected: 0,
            results,
        };
        self.status_msg = "Quick Open (Ctrl+P) — type to filter · Enter open · Esc cancel".into();
    }

    fn open_find(&mut self) {
        self.overlay = Overlay::Find {
            query: String::new(),
            cursor: 0,
            matches: Vec::new(),
            match_idx: 0,
        };
        self.focus = Focus::Editor;
        self.status_msg = "Find (Ctrl+F) — type · Enter next · Esc close".into();
    }

    fn open_replace(&mut self) {
        self.overlay = Overlay::Replace {
            find: String::new(),
            repl: String::new(),
            field: 0,
            cursor: 0,
            matches: Vec::new(),
            match_idx: 0,
        };
        self.focus = Focus::Editor;
        self.status_msg =
            "Replace (Ctrl+H) — Tab field · Enter replace one · Ctrl+Enter all · Esc".into();
    }

    fn refresh_find_matches(query: &str, lines: &[String]) -> Vec<(usize, usize)> {
        if query.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::new();
        for (ri, line) in lines.iter().enumerate() {
            let mut start = 0usize;
            while let Some(rel) = line[start..].find(query) {
                let col = line[..start + rel].chars().count();
                out.push((ri, col));
                start += rel + query.len().max(1);
                if start >= line.len() {
                    break;
                }
            }
        }
        out
    }

    fn goto_match(&mut self, row: usize, col: usize) {
        self.focus = Focus::Editor;
        self.buf_mut().cursor_row = row.min(self.buf().lines.len().saturating_sub(1));
        let len = self.buf().lines.get(row).map(|l| l.chars().count()).unwrap_or(0);
        self.buf_mut().cursor_col = col.min(len);
        let rows = self.layout.content_rows.max(1) as usize;
        let cols = self
            .layout
            .term_w
            .saturating_sub(self.layout.editor_text_col.saturating_sub(1))
            .max(1) as usize;
        self.buf_mut().ensure_visible(rows, cols);
    }

    fn replace_one_at(&mut self, row: usize, col: usize, find: &str, repl: &str) -> bool {
        if find.is_empty() {
            return false;
        }
        self.buf_mut().push_undo_point();
        let Some(line) = self.buf().lines.get(row).cloned() else {
            return false;
        };
        let byte_start = char_index_to_byte(&line, col);
        let byte_end = byte_start + find.len();
        if byte_end > line.len() || &line[byte_start..byte_end] != find {
            // try re-find on line from char col
            if let Some(rel) = line[byte_start.min(line.len())..].find(find) {
                let bs = byte_start + rel;
                let be = bs + find.len();
                let mut new_line = line.clone();
                new_line.replace_range(bs..be, repl);
                let old_end_col = col + find.chars().count();
                self.buf_mut().lines[row] = new_line;
                self.buf_mut().dirty = true;
                let path = self.buf().path.clone();
                let lines = self.buf().lines.clone();
                self.buf_mut().highlight.apply_edit(
                    row,
                    col,
                    row,
                    old_end_col,
                    repl,
                    &lines,
                );
                let _ = path;
                return true;
            }
            return false;
        }
        let mut new_line = line;
        new_line.replace_range(byte_start..byte_end, repl);
        let old_end_col = col + find.chars().count();
        self.buf_mut().lines[row] = new_line;
        self.buf_mut().dirty = true;
        let lines = self.buf().lines.clone();
        self.buf_mut()
            .highlight
            .apply_edit(row, col, row, old_end_col, repl, &lines);
        true
    }

    fn run_menu_action(&mut self, action: MenuAction) {
        match action {
            MenuAction::FileSave => self.do_save(),
            MenuAction::FileOpen | MenuAction::GoQuickOpen => self.open_quick_open(),
            MenuAction::EditFind => self.open_find(),
            MenuAction::EditReplace => self.open_replace(),
            MenuAction::ViewExplorer => {
                self.focus = Focus::Sidebar;
                self.overlay = Overlay::None;
                self.status_msg = "Explorer focused".into();
            }
            MenuAction::Terminal => self.toggle_terminal_panel(),
            MenuAction::Help => self.show_help = true,
            MenuAction::Quit => self.request_quit(false),
        }
    }

    pub fn handle_input(&mut self, input: Input) {
        match input {
            Input::Key(k) => self.handle_key(k),
            Input::Mouse(m) => self.handle_mouse(m),
        }
    }

    pub fn handle_mouse(&mut self, ev: MouseEvent) {
        if self.show_help {
            if ev.action == MouseAction::Down && ev.button == MouseButton::Left {
                self.show_help = false;
            }
            return;
        }

        // Overlay: click outside could close — ignore for simplicity
        if !matches!(self.overlay, Overlay::None) {
            return;
        }

        let lay = self.layout;
        let row = ev.row;
        let col = ev.col;

        // Menu bar clicks
        if row == lay.menu_row
            && ev.action == MouseAction::Down
            && ev.button == MouseButton::Left
        {
            for item in &self.menu_items.clone() {
                if col >= item.col_start && col <= item.col_end {
                    self.run_menu_action(item.action);
                    return;
                }
            }
            return;
        }


        // Terminal panel chrome / body
        if self.shell_panel.visible {
            let panel_top = self.layout.term_h.saturating_sub(
                self.shell_panel.height.min(self.layout.term_h.saturating_sub(6)).max(5),
            );
            if row >= panel_top && row < self.layout.term_h {
                if ev.action == MouseAction::Down && ev.button == MouseButton::Left {
                    if row == panel_top {
                        for &(a, b, hit) in &self.term_tab_hits.clone() {
                            if col >= a && col <= b {
                                match hit {
                                    TermHit::Tab(i) => {
                                        self.shell_panel.active = i;
                                        self.focus = Focus::Terminal;
                                        self.shell_panel.focus = true;
                                    }
                                    TermHit::Add => self.add_terminal_tab(),
                                    TermHit::Close => {
                                        self.shell_panel.close_active();
                                        if self.shell_panel.sessions.is_empty() {
                                            self.shell_panel.visible = false;
                                            self.focus = Focus::Editor;
                                        }
                                    }
                                }
                                return;
                            }
                        }
                    } else {
                        // click body → focus terminal
                        self.focus = Focus::Terminal;
                        self.shell_panel.focus = true;
                        self.completion = None;
                    }
                }
                if ev.action == MouseAction::Scroll {
                    if let Some(s) = self.shell_panel.active_mut() {
                        if matches!(ev.button, MouseButton::WheelUp) {
                            s.scroll = s.scroll.saturating_sub(3);
                        } else {
                            s.scroll = s.scroll.saturating_add(3);
                        }
                    }
                }
                return;
            }
        }

        // Scroll wheel
        if ev.action == MouseAction::Scroll
            || matches!(ev.button, MouseButton::WheelUp | MouseButton::WheelDown)
        {
            let up = matches!(ev.button, MouseButton::WheelUp);
            if col <= lay.side_w {
                self.focus = Focus::Sidebar;
                if up {
                    self.tree.select_prev();
                } else {
                    self.tree.select_next();
                }
            } else {
                self.focus = Focus::Editor;
                if up {
                    self.buf_mut().move_up();
                } else {
                    self.buf_mut().move_down();
                }
            }
            return;
        }

        let is_down = ev.action == MouseAction::Down;
        let is_drag = ev.action == MouseAction::Drag;
        if !is_down && !is_drag {
            return;
        }
        if !matches!(ev.button, MouseButton::Left | MouseButton::Right) && !is_drag {
            return;
        }

        if row < lay.content_top || row >= lay.content_top + lay.content_rows {
            if row == lay.tab_row && col <= lay.side_w && is_down {
                self.focus = Focus::Sidebar;
            }
            return;
        }

        let view_row = (row - lay.content_top) as usize;

        if col <= lay.side_w {
            self.focus = Focus::Sidebar;
            let idx = self.tree.scroll + view_row;
            if idx < self.tree.entries.len() {
                self.tree.selected = idx;
                self.tree.ensure_visible(lay.content_rows as usize);
                if is_down && ev.button == MouseButton::Left {
                    let now = Instant::now();
                    let double = self
                        .last_tree_click
                        .map(|(i, t)| i == idx && now.duration_since(t).as_millis() < 400)
                        .unwrap_or(false);
                    self.last_tree_click = Some((idx, now));
                    if self.tree.selected_is_dir() {
                        if double {
                            self.tree.toggle();
                        }
                    } else {
                        self.open_selected();
                    }
                }
            }
            return;
        }

        // Editor text / gutter — fix cursor: text col is 1-based cell of char index 0
        if col >= lay.editor_text_col || (col > lay.side_w && col < lay.editor_text_col) {
            self.focus = Focus::Editor;
            let file_row = self.buf().scroll_row + view_row;
            let nlines = self.buf().lines.len();
            if nlines == 0 {
                return;
            }
            let file_row = file_row.min(nlines - 1);
            self.buf_mut().cursor_row = file_row;
            let line_len = self.buf().lines[file_row].chars().count();

            if col >= lay.editor_text_col {
                // Click on character cell → cursor **before** that character (insertion point).
                // Terminal cell at editor_text_col is char 0; cursor sits at insertion column.
                let cells_from_start = (col - lay.editor_text_col) as usize;
                let abs_col = self.buf().scroll_col + cells_from_start;
                // Place cursor at clicked cell (not one behind): insertion at that char index
                self.buf_mut().cursor_col = abs_col.min(line_len);
            } else {
                // gutter → line start
                self.buf_mut().cursor_col = 0;
            }

            let edit_cols = lay
                .term_w
                .saturating_sub(lay.editor_text_col.saturating_sub(1))
                .max(1) as usize;
            self.buf_mut()
                .ensure_visible(lay.content_rows as usize, edit_cols);
            if is_down {
                self.status_msg = format!(
                    "Cursor Ln {}, Col {}",
                    self.buf().cursor_row + 1,
                    self.buf().cursor_col + 1
                );
            }
        }
    }

    pub fn handle_key(&mut self, key: Key) {
        // Overlay takes priority (except force quit / save)
        if !matches!(self.overlay, Overlay::None) {
            match key {
                Key::Ctrl('c') => {
                    self.request_quit(true);
                    return;
                }
                Key::Ctrl('s') | Key::Ctrl('S') => {
                    self.do_save();
                    return;
                }
                Key::Ctrl('z') | Key::Ctrl('Z') => {
                    self.completion = None;
                    let _ = self.buf_mut().undo();
                    return;
                }
                Key::Ctrl('y') | Key::Ctrl('Y') => {
                    self.completion = None;
                    let _ = self.buf_mut().redo();
                    return;
                }
                _ => {}
            }
            self.handle_overlay_key(key);
            return;
        }

        match key {
            Key::Ctrl('c') => {
                self.request_quit(true);
                return;
            }
            Key::Ctrl('q') | Key::Ctrl('d') => {
                self.request_quit(false);
                return;
            }
            Key::Ctrl('s') | Key::Ctrl('S') => {
                self.do_save();
                return;
            }
            // Undo / Redo (Ctrl+Z / Ctrl+Y; Ctrl+Shift+Z often unavailable in terminals)
            Key::Ctrl('z') | Key::Ctrl('Z') => {
                self.completion = None;
                if self.buf_mut().undo() {
                    self.status_msg = "Undo".into();
                } else {
                    self.status_msg = "Nothing to undo".into();
                }
                return;
            }
            Key::Ctrl('y') | Key::Ctrl('Y') => {
                self.completion = None;
                if self.buf_mut().redo() {
                    self.status_msg = "Redo".into();
                } else {
                    self.status_msg = "Nothing to redo".into();
                }
                return;
            }
            // Toggle integrated terminal (Ctrl+J = 0x0A — must not be parsed as Enter).
            // Also accept Ctrl+\\ as a backup (some terminals steal Ctrl+J).
            Key::Ctrl('j') | Key::Ctrl('J') | Key::Ctrl('\\') => {
                self.toggle_terminal_panel();
                return;
            }
            // VS Code: Ctrl+P quick open (was prev tab)
            Key::Ctrl('p') | Key::Ctrl('P') => {
                self.open_quick_open();
                return;
            }
            // VS Code: Ctrl+F find
            Key::Ctrl('f') | Key::Ctrl('F') => {
                self.open_find();
                return;
            }
            // VS Code: Ctrl+H replace (was help — help is now F1 / ?)
            Key::Ctrl('h') | Key::Ctrl('H') => {
                self.open_replace();
                return;
            }
            _ => {}
        }

        if self.show_help {
            match key {
                Key::Esc | Key::Char('q') | Key::Char('?') | Key::Enter => {
                    self.show_help = false;
                }
                _ => {}
            }
            return;
        }

        // Autocomplete popup has priority in the editor
        if self.completion.is_some() && self.focus == Focus::Editor {
            match key {
                Key::Esc => {
                    self.dismiss_completion();
                    return;
                }
                Key::Up => {
                    if let Some(c) = self.completion.as_mut() {
                        c.selected = c.selected.saturating_sub(1);
                    }
                    return;
                }
                Key::Down => {
                    if let Some(c) = self.completion.as_mut() {
                        if c.selected + 1 < c.items.len() {
                            c.selected += 1;
                        }
                    }
                    return;
                }
                Key::Enter | Key::Tab => {
                    // Tab accepts completion when popup is open (doesn't switch focus)
                    self.accept_completion();
                    return;
                }
                Key::Ctrl(' ') => {
                    // force refresh
                    self.refresh_completion();
                    return;
                }
                _ => {
                    // fall through to editor — will refresh completion after
                }
            }
        }


        // Integrated terminal captures most keys when focused
        if self.focus == Focus::Terminal && self.shell_panel.visible {
            match key {
                Key::Ctrl('j') | Key::Ctrl('J') | Key::Ctrl('\\') => {
                    self.toggle_terminal_panel();
                    return;
                }
                Key::Ctrl('c') => {
                    // send to shell, not quit app (quit is Ctrl+Q)
                    if let Some(s) = self.shell_panel.active_mut() {
                        s.write_key_bytes(&[0x03]);
                    }
                    return;
                }
                Key::Ctrl('q') | Key::Ctrl('d') => { /* fall through to global — already handled */ }
                Key::Ctrl('n') | Key::Ctrl('N') => {
                    self.add_terminal_tab();
                    return;
                }
                Key::Ctrl('w') => {
                    self.shell_panel.close_active();
                    if self.shell_panel.sessions.is_empty() {
                        self.shell_panel.visible = false;
                        self.focus = Focus::Editor;
                    }
                    return;
                }
                Key::Ctrl('t') | Key::Ctrl('T') => {
                    self.shell_panel.next_tab();
                    return;
                }
                Key::Esc => {
                    self.focus = Focus::Editor;
                    self.shell_panel.focus = false;
                    self.status_msg = "Editor focused (Ctrl+J terminal)".into();
                    return;
                }
                other => {
                    if let Some(bytes) = key_to_pty_bytes(&other) {
                        if let Some(s) = self.shell_panel.active_mut() {
                            s.write_key_bytes(&bytes);
                        }
                    }
                    return;
                }
            }
        }

        if matches!(key, Key::Ctrl(' ')) {
            self.focus = Focus::Editor;
            self.refresh_completion();
            if self.completion.is_none() {
                self.status_msg = "No completions for prefix under cursor".into();
            } else {
                self.status_msg = "Completions — ↑↓ select · Tab/Enter accept · Esc dismiss".into();
            }
            return;
        }

        if !matches!(key, Key::Ctrl('q') | Key::Ctrl('d')) {
            self.pending_quit_confirm = false;
        }

        match key {
            Key::Ctrl('b') => {
                self.focus = match self.focus {
                    Focus::Sidebar => Focus::Editor,
                    Focus::Editor => Focus::Sidebar,
                    Focus::Terminal => Focus::Editor,
                };
                self.shell_panel.focus = false;
                self.completion = None;
            }
            Key::Tab if self.completion.is_none() => {
                // cycle: explorer → editor → terminal → explorer
                self.focus = match self.focus {
                    Focus::Sidebar => Focus::Editor,
                    Focus::Editor => {
                        if self.shell_panel.visible {
                            self.shell_panel.focus = true;
                            Focus::Terminal
                        } else {
                            Focus::Sidebar
                        }
                    }
                    Focus::Terminal => {
                        self.shell_panel.focus = false;
                        Focus::Sidebar
                    }
                };
                self.completion = None;
            }
            Key::Ctrl('w') => self.close_tab(),
            Key::Ctrl('t') | Key::Ctrl('n') => self.next_tab(),
            // prev tab: Ctrl+Shift+Tab not available — use Ctrl+[
            Key::Ctrl('[') => self.prev_tab(),
            Key::Ctrl('r') => {
                self.tree.refresh();
                self.rebuild_file_index();
                self.focus = Focus::Sidebar;
                self.status_msg = "Explorer + file index refreshed".into();
            }
            Key::Char('?') => self.show_help = true,
            Key::Esc => {
                if self.completion.take().is_some() {
                    return;
                }
                self.focus = Focus::Sidebar;
                self.status_msg = "Explorer — ↑↓ · Enter · Ctrl+P files".into();
            }
            Key::Char('q') if self.focus == Focus::Sidebar => self.request_quit(false),
            other => {
                let refresh_after = matches!(
                    &other,
                    Key::Char(c) if complete::is_ident_char(*c)
                ) || matches!(
                    &other,
                    Key::Backspace | Key::Delete | Key::Left | Key::Right | Key::Home | Key::End
                );
                let close_after = matches!(
                    &other,
                    Key::Up | Key::Down | Key::Enter | Key::PageUp | Key::PageDown
                );
                match self.focus {
                    Focus::Sidebar => {
                        self.completion = None;
                        self.handle_sidebar(other);
                    }
                    Focus::Editor => {
                        self.handle_editor(other);
                        if refresh_after {
                            self.refresh_completion();
                        } else if close_after {
                            self.completion = None;
                        }
                    }
                    Focus::Terminal => {
                        // should have been handled above; safety net
                        if let Some(bytes) = key_to_pty_bytes(&other) {
                            if let Some(s) = self.shell_panel.active_mut() {
                                s.write_key_bytes(&bytes);
                            }
                        }
                    }
                }
            }
        }
    }

    fn handle_overlay_key(&mut self, key: Key) {
        // take overlay to avoid borrow issues
        let mut overlay = std::mem::replace(&mut self.overlay, Overlay::None);
        match &mut overlay {
            Overlay::None => {}
            Overlay::QuickOpen {
                query,
                cursor,
                selected,
                results,
            } => match key {
                Key::Esc => {
                    self.status_msg = "Quick Open cancelled".into();
                    return;
                }
                Key::Enter => {
                    if let Some(p) = results.get(*selected).cloned() {
                        self.open_path(p);
                    }
                    return;
                }
                Key::Up => {
                    *selected = selected.saturating_sub(1);
                }
                Key::Down => {
                    if *selected + 1 < results.len() {
                        *selected += 1;
                    }
                }
                Key::Backspace => {
                    if *cursor > 0 {
                        let b = char_index_to_byte(query, *cursor - 1);
                        let e = char_index_to_byte(query, *cursor);
                        query.replace_range(b..e, "");
                        *cursor -= 1;
                        *results = filter_files(&self.file_index, query);
                        *selected = 0;
                    }
                }
                Key::Left => *cursor = cursor.saturating_sub(1),
                Key::Right => {
                    if *cursor < query.chars().count() {
                        *cursor += 1;
                    }
                }
                Key::Char(c) if !c.is_control() => {
                    let b = char_index_to_byte(query, *cursor);
                    query.insert(b, c);
                    *cursor += 1;
                    *results = filter_files(&self.file_index, query);
                    *selected = 0;
                }
                _ => {}
            },
            Overlay::Find {
                query,
                cursor,
                matches,
                match_idx,
            } => match key {
                Key::Esc => {
                    self.status_msg = "Find closed".into();
                    return;
                }
                Key::Enter | Key::Down => {
                    if !matches.is_empty() {
                        *match_idx = (*match_idx + 1) % matches.len();
                        let (r, c) = matches[*match_idx];
                        self.goto_match(r, c);
                        self.status_msg =
                            format!("Find {}/{}", *match_idx + 1, matches.len());
                    }
                }
                Key::Up => {
                    if !matches.is_empty() {
                        *match_idx = if *match_idx == 0 {
                            matches.len() - 1
                        } else {
                            *match_idx - 1
                        };
                        let (r, c) = matches[*match_idx];
                        self.goto_match(r, c);
                    }
                }
                Key::Backspace => {
                    if *cursor > 0 {
                        let b = char_index_to_byte(query, *cursor - 1);
                        let e = char_index_to_byte(query, *cursor);
                        query.replace_range(b..e, "");
                        *cursor -= 1;
                        *matches = Self::refresh_find_matches(query, &self.buffers[self.active].lines);
                        *match_idx = 0;
                        if let Some(&(r, c)) = matches.first() {
                            self.goto_match(r, c);
                        }
                    }
                }
                Key::Left => *cursor = cursor.saturating_sub(1),
                Key::Right => {
                    if *cursor < query.chars().count() {
                        *cursor += 1;
                    }
                }
                Key::Char(c) if !c.is_control() => {
                    let b = char_index_to_byte(query, *cursor);
                    query.insert(b, c);
                    *cursor += 1;
                    *matches = Self::refresh_find_matches(query, &self.buffers[self.active].lines);
                    *match_idx = 0;
                    if let Some(&(r, c)) = matches.first() {
                        self.goto_match(r, c);
                    }
                    self.status_msg = format!("{} matches", matches.len());
                }
                _ => {}
            },
            Overlay::Replace {
                find,
                repl,
                field,
                cursor,
                matches,
                match_idx,
            } => match key {
                Key::Esc => {
                    self.status_msg = "Replace closed".into();
                    return;
                }
                Key::Tab => {
                    *field = 1 - *field;
                    *cursor = if *field == 0 {
                        find.chars().count()
                    } else {
                        repl.chars().count()
                    };
                }
                // Ctrl+Enter → replace all (Ctrl+J is 0x0a — use Ctrl+A for all as alt)
                Key::Ctrl('a') | Key::Ctrl('A') => {
                    let f = find.clone();
                    let r = repl.clone();
                    if !f.is_empty() {
                        let mut n = 0usize;
                        // replace from end so indices stay valid
                        let mut ms = Self::refresh_find_matches(&f, &self.buffers[self.active].lines);
                        ms.reverse();
                        for (row, col) in ms {
                            if self.replace_one_at(row, col, &f, &r) {
                                n += 1;
                            }
                        }
                        *matches = Self::refresh_find_matches(&f, &self.buffers[self.active].lines);
                        *match_idx = 0;
                        self.status_msg = format!("Replaced {n} occurrence(s)");
                    }
                }
                Key::Enter => {
                    let f = find.clone();
                    let r = repl.clone();
                    if !f.is_empty() {
                        if matches.is_empty() {
                            *matches =
                                Self::refresh_find_matches(&f, &self.buffers[self.active].lines);
                            *match_idx = 0;
                        }
                        if !matches.is_empty() {
                            let mi = (*match_idx).min(matches.len() - 1);
                            let (row, col) = matches[mi];
                            if self.replace_one_at(row, col, &f, &r) {
                                *matches = Self::refresh_find_matches(
                                    &f,
                                    &self.buffers[self.active].lines,
                                );
                                if *match_idx >= matches.len() && !matches.is_empty() {
                                    *match_idx = matches.len() - 1;
                                }
                                if let Some(&(nr, nc)) = matches.get(*match_idx) {
                                    self.goto_match(nr, nc);
                                }
                                self.status_msg = format!(
                                    "Replaced 1 — {} left (Ctrl+A = replace all)",
                                    matches.len()
                                );
                            }
                        }
                    }
                }
                Key::Backspace => {
                    let s = if *field == 0 { &mut *find } else { &mut *repl };
                    if *cursor > 0 {
                        let b = char_index_to_byte(s, *cursor - 1);
                        let e = char_index_to_byte(s, *cursor);
                        s.replace_range(b..e, "");
                        *cursor -= 1;
                    }
                    if *field == 0 {
                        let q = find.clone();
                        *matches = Self::refresh_find_matches(
                            &q,
                            &self.buffers[self.active].lines,
                        );
                        *match_idx = 0;
                    }
                }
                Key::Left => *cursor = cursor.saturating_sub(1),
                Key::Right => {
                    let len = if *field == 0 {
                        find.chars().count()
                    } else {
                        repl.chars().count()
                    };
                    if *cursor < len {
                        *cursor += 1;
                    }
                }
                Key::Char(c) if !c.is_control() => {
                    let s = if *field == 0 { &mut *find } else { &mut *repl };
                    let b = char_index_to_byte(s, *cursor);
                    s.insert(b, c);
                    *cursor += 1;
                    if *field == 0 {
                        let q = find.clone();
                        *matches = Self::refresh_find_matches(
                            &q,
                            &self.buffers[self.active].lines,
                        );
                        *match_idx = 0;
                        if let Some(&(r, c)) = matches.first() {
                            self.goto_match(r, c);
                        }
                    }
                }
                Key::Down if !matches.is_empty() => {
                    *match_idx = (*match_idx + 1) % matches.len();
                    let (r, c) = matches[*match_idx];
                    self.goto_match(r, c);
                }
                Key::Up if !matches.is_empty() => {
                    *match_idx = if *match_idx == 0 {
                        matches.len() - 1
                    } else {
                        *match_idx - 1
                    };
                    let (r, c) = matches[*match_idx];
                    self.goto_match(r, c);
                }
                _ => {}
            },
        }
        self.overlay = overlay;
    }


    fn draw_terminal_panel(
        &mut self,
        term: &mut Terminal,
        w: u16,
        h: u16,
        panel_h: u16,
    ) -> io::Result<()> {
        let panel_top = h.saturating_sub(panel_h);
        let body_rows = panel_h.saturating_sub(1).max(1);
        let cols = w.saturating_sub(2).max(20);
        self.shell_panel.resize_all(cols, body_rows);
        self.term_tab_hits.clear();

        // Header: tabs + [+] + [x]
        term.move_to(panel_top, 1)?;
        term.set_bg(37, 37, 38)?;
        term.set_fg(180, 180, 180)?;
        let mut col: u16 = 1;
        term.write_str(" TERMINAL ")?;
        col += 10;

        for (i, _s) in self.shell_panel.sessions.iter().enumerate() {
            let label = if i == self.shell_panel.active {
                format!(" [{}] ", self.shell_panel.sessions[i].title)
            } else {
                format!("  {}  ", self.shell_panel.sessions[i].title)
            };
            let start = col;
            if i == self.shell_panel.active {
                term.set_bg(30, 30, 30)?;
                term.set_fg(255, 255, 255)?;
            } else {
                term.set_bg(45, 45, 48)?;
                term.set_fg(160, 160, 160)?;
            }
            term.write_str(&label)?;
            let end = start + label.chars().count() as u16 - 1;
            self.term_tab_hits.push((start, end, TermHit::Tab(i)));
            col = end + 1;
            if col + 8 >= w {
                break;
            }
        }

        // [+] new terminal
        let plus = " [+] ";
        let ps = col;
        term.set_bg(0, 122, 204)?;
        term.set_fg(255, 255, 255)?;
        term.write_str(plus)?;
        let pe = ps + plus.chars().count() as u16 - 1;
        self.term_tab_hits.push((ps, pe, TermHit::Add));
        col = pe + 1;

        // close tab
        let cls = " [x] ";
        let cs = col;
        term.set_bg(90, 40, 40)?;
        term.set_fg(255, 200, 200)?;
        term.write_str(cls)?;
        let ce = cs + cls.chars().count() as u16 - 1;
        self.term_tab_hits.push((cs, ce, TermHit::Close));
        col = ce + 1;

        if col <= w {
            term.set_bg(37, 37, 38)?;
            let pad = (w as usize).saturating_sub(col as usize - 1).min(200);
            term.write_str(&" ".repeat(pad))?;
        }
        term.reset_style()?;

        // Body
        let lines = self
            .shell_panel
            .active_mut()
            .map(|s| {
                let all = s.display_lines();
                let scroll = s.scroll;
                all
                    .into_iter()
                    .skip(scroll)
                    .take(body_rows as usize)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        for row_i in 0..body_rows as usize {
            let screen_row = panel_top + 1 + row_i as u16;
            if screen_row >= h {
                break;
            }
            term.move_to(screen_row, 1)?;
            let focused = self.focus == Focus::Terminal;
            if focused {
                term.set_bg(20, 20, 20)?;
                term.set_fg(200, 200, 200)?;
            } else {
                term.set_bg(25, 25, 25)?;
                term.set_fg(140, 140, 140)?;
            }
            let line = lines.get(row_i).map(|s| s.as_str()).unwrap_or("");
            let mut cell = pad_or_trunc(line, w as usize);
            if focused && row_i + 1 == lines.len().min(body_rows as usize) {
                // caret hint
                if cell.chars().count() < w as usize {
                    // show block at end of last visible line
                }
            }
            term.write_str(&cell)?;
            term.reset_style()?;
        }
        Ok(())
    }

    fn handle_sidebar(&mut self, key: Key) {
        match key {
            Key::Up | Key::Char('k') | Key::Char('K') => {
                self.tree.select_prev();
                self.status_selected();
            }
            Key::Down | Key::Char('j') | Key::Char('J') => {
                self.tree.select_next();
                self.status_selected();
            }
            Key::Enter => self.open_selected(),
            Key::Right | Key::Char('l') | Key::Char('L') => {
                if self.tree.selected_is_dir() {
                    if let Some(e) = self.tree.entries.get(self.tree.selected) {
                        if !e.expanded {
                            self.tree.toggle();
                        } else {
                            self.tree.select_next();
                            self.status_selected();
                        }
                    }
                } else {
                    self.open_selected();
                }
            }
            Key::Left | Key::Char('h') | Key::Char('H') => {
                if self.tree.selected_is_dir() {
                    if let Some(e) = self.tree.entries.get(self.tree.selected) {
                        if e.expanded {
                            self.tree.toggle();
                            return;
                        }
                    }
                }
                if let Some(cur) = self.tree.entries.get(self.tree.selected) {
                    let depth = cur.depth;
                    if depth > 0 {
                        let mut i = self.tree.selected;
                        while i > 0 {
                            i -= 1;
                            if self.tree.entries[i].depth < depth {
                                self.tree.selected = i;
                                self.status_selected();
                                break;
                            }
                        }
                    }
                }
            }
            Key::Char(' ') => {
                if self.tree.selected_is_dir() {
                    self.tree.toggle();
                } else {
                    self.open_selected();
                }
            }
            Key::PageUp => {
                for _ in 0..10 {
                    self.tree.select_prev();
                }
                self.status_selected();
            }
            Key::PageDown => {
                for _ in 0..10 {
                    self.tree.select_next();
                }
                self.status_selected();
            }
            Key::Home => {
                self.tree.selected = 0;
                self.status_selected();
            }
            Key::End => {
                if !self.tree.entries.is_empty() {
                    self.tree.selected = self.tree.entries.len() - 1;
                }
                self.status_selected();
            }
            _ => {}
        }
    }

    fn status_selected(&mut self) {
        if let Some(e) = self.tree.entries.get(self.tree.selected) {
            let kind = if e.is_dir { "dir" } else { "file" };
            self.status_msg = format!(
                "[{}/{}] {} ({kind})",
                self.tree.selected + 1,
                self.tree.entries.len(),
                e.name
            );
        }
    }

    fn handle_editor(&mut self, key: Key) {
        let page = 20usize;
        match key {
            Key::Left => self.buf_mut().move_left(),
            Key::Right => self.buf_mut().move_right(),
            Key::Up => self.buf_mut().move_up(),
            Key::Down => self.buf_mut().move_down(),
            Key::Home => self.buf_mut().move_home(),
            Key::End => self.buf_mut().move_end(),
            Key::PageUp => self.buf_mut().page_up(page),
            Key::PageDown => self.buf_mut().page_down(page),
            Key::Backspace => self.buf_mut().backspace(),
            Key::Delete => self.buf_mut().delete(),
            Key::Enter => self.buf_mut().insert_newline(),
            Key::Char(c) if !c.is_control() => self.buf_mut().insert_char(c),
            _ => {}
        }
    }

    pub fn draw(&mut self, term: &mut Terminal) -> io::Result<()> {
        term.refresh_size();
        let w = term.width.max(40);
        let h = term.height.max(10);
        term.write_str("\x1b[H\x1b[J")?;

        let side_w = self.sidebar_width.min(w / 2).max(16);
        let border_col = side_w + 1;
        let menu_row = 1u16;
        let tab_row = 2u16;
        // content below menu + tabs
        let content_top = 3u16;
        let panel_h = if self.shell_panel.visible {
            self.shell_panel.height.min(h.saturating_sub(6)).max(5)
        } else {
            0
        };
        // status at row h; panel sits just above it
        let content_bottom = h.saturating_sub(1 + panel_h);
        let content_rows = content_bottom.saturating_sub(content_top) + 1;

        self.tree.ensure_visible(content_rows as usize);
        let gutter = line_num_width(self.buf().lines.len()) + 1;
        // text starts after border (1 col) + gutter
        let editor_text_col = border_col + gutter as u16;
        let edit_cols = w.saturating_sub(editor_text_col.saturating_sub(1)).max(1) as usize;
        self.buf_mut()
            .ensure_visible(content_rows as usize, edit_cols);

        self.layout = Layout {
            term_w: w,
            term_h: h,
            side_w,
            content_top,
            content_rows: content_rows as u16,
            gutter: gutter as u16,
            editor_text_col,
            border_col,
            menu_row,
            tab_row,
        };

        // ========== MENU BAR (VS Code–like header actions) ==========
        self.menu_items.clear();
        term.move_to(menu_row, 1)?;
        term.set_bg(Theme::TITLE_BG.0, Theme::TITLE_BG.1, Theme::TITLE_BG.2)?;
        term.set_fg(Theme::TITLE_FG.0, Theme::TITLE_FG.1, Theme::TITLE_FG.2)?;

        let menus: &[(&str, MenuAction)] = &[
            (" File ", MenuAction::FileOpen),
            (" Save ", MenuAction::FileSave),
            (" Edit ", MenuAction::EditFind),
            (" Find ", MenuAction::EditFind),
            (" Replace ", MenuAction::EditReplace),
            (" Go ", MenuAction::GoQuickOpen),
            (" View ", MenuAction::ViewExplorer),
            (" Term ", MenuAction::Terminal),
            (" Help ", MenuAction::Help),
            (" Quit ", MenuAction::Quit),
        ];
        let mut col: u16 = 1;
        // accent strip
        term.set_bg(Theme::ACCENT.0, Theme::ACCENT.1, Theme::ACCENT.2)?;
        term.write_str(" ")?;
        col += 1;
        term.set_bg(Theme::TITLE_BG.0, Theme::TITLE_BG.1, Theme::TITLE_BG.2)?;
        term.set_fg(220, 220, 220)?;
        term.write_str(" tui-code ")?;
        col += 10;

        for &(label, action) in menus {
            let label_len = label.chars().count() as u16;
            if col.saturating_add(label_len) > w {
                break;
            }
            let start = col;
            term.set_bg(Theme::TITLE_BG.0, Theme::TITLE_BG.1, Theme::TITLE_BG.2)?;
            term.set_fg(Theme::TITLE_ACTIVE_FG.0, Theme::TITLE_ACTIVE_FG.1, Theme::TITLE_ACTIVE_FG.2)?;
            term.write_str(label)?;
            let end = start.saturating_add(label_len).saturating_sub(1);
            self.menu_items.push(MenuItem {
                label,
                col_start: start,
                col_end: end,
                action,
            });
            col = end.saturating_add(1);
        }
        // fill rest of menu row (never underflow — was causing capacity overflow panic)
        if col <= w {
            let pad = (w as usize).saturating_sub(col as usize).saturating_add(1);
            let pad = pad.min(512);
            term.set_bg(Theme::TITLE_BG.0, Theme::TITLE_BG.1, Theme::TITLE_BG.2)?;
            term.write_str(&spaces(pad))?;
        }
        term.reset_style()?;

        // ========== TAB ROW ==========
        term.move_to(tab_row, 1)?;
        let explorer_label = if self.focus == Focus::Sidebar {
            "▸ EXPLORER"
        } else {
            "  EXPLORER"
        };
        let explorer = pad_or_trunc(explorer_label, side_w as usize);
        if self.focus == Focus::Sidebar {
            term.set_bg(Theme::SIDEBAR_SEL_BG.0, Theme::SIDEBAR_SEL_BG.1, Theme::SIDEBAR_SEL_BG.2)?;
            term.set_fg(255, 255, 255)?;
        } else {
            term.set_bg(Theme::SIDEBAR_BG.0, Theme::SIDEBAR_BG.1, Theme::SIDEBAR_BG.2)?;
            term.set_fg(Theme::DIM.0, Theme::DIM.1, Theme::DIM.2)?;
        }
        term.write_str(&explorer)?;
        term.set_fg(Theme::BORDER.0, Theme::BORDER.1, Theme::BORDER.2)?;
        term.set_bg(Theme::EDITOR_BG.0, Theme::EDITOR_BG.1, Theme::EDITOR_BG.2)?;
        term.write_str("│")?;

        let tab_area = w.saturating_sub(side_w + 1) as usize;
        let mut used = 0usize;
        for (i, buf) in self.buffers.iter().enumerate() {
            let mut name = if buf.dirty {
                format!(" ● {} ", buf.title())
            } else {
                format!("   {} ", buf.title())
            };
            if used + name.chars().count() > tab_area && i > 0 {
                break;
            }
            if i == self.active {
                term.set_bg(Theme::TAB_ACTIVE_BG.0, Theme::TAB_ACTIVE_BG.1, Theme::TAB_ACTIVE_BG.2)?;
                term.set_fg(255, 255, 255)?;
            } else {
                term.set_bg(Theme::TAB_INACTIVE_BG.0, Theme::TAB_INACTIVE_BG.1, Theme::TAB_INACTIVE_BG.2)?;
                term.set_fg(Theme::TITLE_FG.0, Theme::TITLE_FG.1, Theme::TITLE_FG.2)?;
            }
            let piece = if used + name.chars().count() > tab_area {
                truncate(&name, tab_area - used)
            } else {
                name
            };
            term.write_str(&piece)?;
            used += piece.chars().count();
        }
        if used < tab_area {
            term.set_bg(Theme::TAB_INACTIVE_BG.0, Theme::TAB_INACTIVE_BG.1, Theme::TAB_INACTIVE_BG.2)?;
            term.write_str(&spaces(tab_area - used))?;
        }
        term.reset_style()?;

        // ========== CONTENT ==========
        let scroll = self.tree.scroll;
        let buf_scroll = self.buf().scroll_row;
        let buf_scroll_col = self.buf().scroll_col;
        let active_row = self.buf().cursor_row;
        let sel = self.tree.selected;

        // Find highlight ranges for current line (optional match mark)
        let find_query = match &self.overlay {
            Overlay::Find { query, .. } => Some(query.clone()),
            Overlay::Replace { find, .. } => Some(find.clone()),
            _ => None,
        };

        for row_i in 0..content_rows as usize {
            let screen_row = content_top + row_i as u16;
            term.move_to(screen_row, 1)?;

            let entry_idx = scroll + row_i;
            if let Some(entry) = self.tree.entries.get(entry_idx) {
                let selected = entry_idx == sel;
                if selected {
                    term.set_bg(Theme::SIDEBAR_SEL_BG.0, Theme::SIDEBAR_SEL_BG.1, Theme::SIDEBAR_SEL_BG.2)?;
                    term.set_fg(255, 255, 255)?;
                } else {
                    term.set_bg(Theme::SIDEBAR_BG.0, Theme::SIDEBAR_BG.1, Theme::SIDEBAR_BG.2)?;
                    if entry.is_dir {
                        term.set_fg(Theme::DIR_FG.0, Theme::DIR_FG.1, Theme::DIR_FG.2)?;
                    } else {
                        term.set_fg(Theme::FILE_FG.0, Theme::FILE_FG.1, Theme::FILE_FG.2)?;
                    }
                }
                let indent = spaces(entry.depth.saturating_mul(2).min(64));
                let icon = if entry.is_dir {
                    if entry.expanded { "▾ " } else { "▸ " }
                } else {
                    "  "
                };
                let mark = if selected && self.focus == Focus::Sidebar { "›" } else { " " };
                let label = format!("{mark}{indent}{icon}{}", entry.name);
                term.write_str(&pad_or_trunc(&label, side_w as usize))?;
            } else {
                term.set_bg(Theme::SIDEBAR_BG.0, Theme::SIDEBAR_BG.1, Theme::SIDEBAR_BG.2)?;
                term.write_str(&spaces(side_w as usize))?;
            }

            term.set_bg(Theme::EDITOR_BG.0, Theme::EDITOR_BG.1, Theme::EDITOR_BG.2)?;
            term.set_fg(Theme::BORDER.0, Theme::BORDER.1, Theme::BORDER.2)?;
            term.write_str("│")?;

            let file_row = buf_scroll + row_i;
            let is_cur = file_row == active_row && self.focus == Focus::Editor;
            let line_bg = if is_cur { Theme::CUR_LINE_BG } else { Theme::EDITOR_BG };
            term.set_bg(line_bg.0, line_bg.1, line_bg.2)?;

            let ln_w = gutter;
            if file_row < self.buf().lines.len() {
                let ln = format!("{:>width$} ", file_row + 1, width = ln_w - 1);
                if is_cur {
                    term.set_fg(Theme::LINE_NUM_ACTIVE.0, Theme::LINE_NUM_ACTIVE.1, Theme::LINE_NUM_ACTIVE.2)?;
                } else {
                    term.set_fg(Theme::LINE_NUM_FG.0, Theme::LINE_NUM_FG.1, Theme::LINE_NUM_FG.2)?;
                }
                term.write_str(&ln)?;

                let line = self.buf().lines[file_row].as_str();
                let spans = self.buf().highlight.spans_for_line(file_row);
                // Optional: boost find matches with yellow bg via special paint
                write_highlighted_line(
                    term,
                    line,
                    &spans,
                    buf_scroll_col,
                    edit_cols,
                    line_bg,
                    find_query.as_deref(),
                )?;
            } else {
                term.set_fg(Theme::DIM.0, Theme::DIM.1, Theme::DIM.2)?;
                term.write_str(&spaces(ln_w.saturating_add(edit_cols).min(512)))?;
            }
            term.reset_style()?;
        }

        // ========== STATUS ==========
        term.move_to(h, 1)?;
        term.set_bg(Theme::STATUS_BG.0, Theme::STATUS_BG.1, Theme::STATUS_BG.2)?;
        term.set_fg(255, 255, 255)?;
        let focus_label = match self.focus {
            Focus::Sidebar => "EXPLORER",
            Focus::Editor => "EDITOR",
            Focus::Terminal => "TERMINAL",
        };
        let dirty = if self.buf().dirty { "*" } else { "" };
        let left = format!(
            " {}{} |{}| Ln {}, Col {} | {} ",
            self.buf().title(),
            dirty,
            focus_label,
            self.buf().cursor_row + 1,
            self.buf().cursor_col + 1,
            self.buf().lang_label()
        );
        let right = " ^Z Undo ^Y Redo ^S Save ^C Quit ";
        let msg = truncate(&self.status_msg, (w as usize / 4).max(8).min(40));
        let used_w = display_width(&left)
            .saturating_add(display_width(right))
            .saturating_add(display_width(&msg));
        let fill = (w as usize).saturating_sub(used_w).max(1).min(512);
        let status = pad_or_trunc(
            &format!("{left}{msg}{}{right}", spaces(fill)),
            w as usize,
        );
        term.write_str(&status)?;
        term.reset_style()?;

        // Integrated terminal panel (VS Code style)
        if self.shell_panel.visible && panel_h > 0 {
            self.draw_terminal_panel(term, w, h, panel_h)?;
        }


        // Autocomplete popup (under cursor in editor)
        if let Some(ref comp) = self.completion {
            if self.focus == Focus::Editor && matches!(self.overlay, Overlay::None) {
                draw_completion_popup(
                    term,
                    comp,
                    content_top,
                    editor_text_col,
                    self.buf().scroll_row,
                    self.buf().scroll_col,
                    self.buf().cursor_row,
                    w,
                    h,
                )?;
            }
        }

        // Overlays
        if !matches!(self.overlay, Overlay::None) {
            draw_overlay(term, &self.overlay, w, h, &self.tree.root)?;
        }
        if self.show_help {
            draw_help(term, w, h)?;
        }

        // Cursor placement — bar at insertion point (NOT one cell behind).
        // Cell `editor_text_col` shows character index `scroll_col`; cursor for
        // `cursor_col` sits on that cell (before that character / at EOL).
        let show_text_cursor = self.focus == Focus::Editor
            && !self.show_help
            && matches!(self.overlay, Overlay::None);
        if show_text_cursor {
            let off = self
                .buf()
                .cursor_col
                .saturating_sub(self.buf().scroll_col);
            let cur_screen_row = content_top
                + (self.buf().cursor_row.saturating_sub(self.buf().scroll_row)) as u16;
            // 1-based: first text column is editor_text_col for off==0
            let cur_screen_col = editor_text_col.saturating_add(off as u16);
            term.show_cursor()?;
            term.set_cursor_shape_bar()?;
            term.move_to(cur_screen_row, cur_screen_col.min(w))?;
        } else if matches!(self.overlay, Overlay::None) {
            term.hide_cursor()?;
        } else {
            // overlay has its own field cursor approximation — hide block in editor
            term.hide_cursor()?;
        }

        term.flush()?;
        Ok(())
    }
}

fn walk_files(root: &Path, dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 12 || out.len() > 8000 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for ent in rd.flatten() {
        let path = ent.path();
        let name = ent.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || name == "target" || name == "node_modules" {
            continue;
        }
        if path.is_dir() {
            walk_files(root, &path, out, depth + 1);
        } else {
            out.push(path);
        }
    }
}

fn filter_files(all: &[PathBuf], query: &str) -> Vec<PathBuf> {
    let q = query.to_ascii_lowercase();
    let mut v: Vec<PathBuf> = all
        .iter()
        .filter(|p| {
            if q.is_empty() {
                return true;
            }
            p.to_string_lossy().to_ascii_lowercase().contains(&q)
        })
        .cloned()
        .collect();
    v.truncate(200);
    v
}

fn char_index_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

fn write_highlighted_line(
    term: &mut Terminal,
    line: &str,
    spans: &[crate::highlight::Span],
    scroll_col: usize,
    width: usize,
    bg: (u8, u8, u8),
    find_q: Option<&str>,
) -> io::Result<()> {
    let char_bytes: Vec<(usize, char)> = line.char_indices().collect();
    let total_chars = char_bytes.len();
    let mut painted = 0usize;
    let mut char_i = scroll_col.min(total_chars);

    // Precompute which char indices are inside a find match (safe; no slice panics).
    let mut in_find = vec![false; total_chars];
    if let Some(q) = find_q.filter(|q| !q.is_empty()) {
        let q_chars: Vec<char> = q.chars().collect();
        let qlen = q_chars.len();
        if qlen > 0 && qlen <= total_chars {
            let line_chars: Vec<char> = char_bytes.iter().map(|(_, c)| *c).collect();
            let max_start = total_chars - qlen;
            for start in 0..=max_start {
                if line_chars[start..start + qlen] == q_chars[..] {
                    for i in start..start + qlen {
                        if i < in_find.len() {
                            in_find[i] = true;
                        }
                    }
                }
            }
        }
    }

    while painted < width && char_i < total_chars {
        let (byte_start, ch) = char_bytes[char_i];
        let mut fg = span_fg_at(spans, byte_start).unwrap_or(Theme::EDITOR_FG);
        let mut cell_bg = bg;
        if in_find.get(char_i).copied().unwrap_or(false) {
            cell_bg = (61, 61, 0);
            fg = (255, 255, 0);
        }
        term.set_bg(cell_bg.0, cell_bg.1, cell_bg.2)?;
        term.set_fg(fg.0, fg.1, fg.2)?;
        let mut buf = [0u8; 4];
        term.write_str(ch.encode_utf8(&mut buf))?;
        painted += 1;
        char_i += 1;
    }
    if painted < width {
        term.set_bg(bg.0, bg.1, bg.2)?;
        term.write_str(&spaces(width.saturating_sub(painted)))?;
    }
    Ok(())
}

/// Safe padding — never allocate huge strings (avoids capacity overflow panics).
fn spaces(n: usize) -> String {
    let n = n.min(1024);
    " ".repeat(n)
}

fn span_fg_at(spans: &[crate::highlight::Span], byte: usize) -> Option<(u8, u8, u8)> {
    for s in spans {
        if byte >= s.start_byte && byte < s.end_byte {
            return Some(s.fg);
        }
    }
    None
}

fn draw_completion_popup(
    term: &mut Terminal,
    comp: &CompletionState,
    content_top: u16,
    editor_text_col: u16,
    scroll_row: usize,
    scroll_col: usize,
    cursor_row: usize,
    term_w: u16,
    term_h: u16,
) -> io::Result<()> {
    let max_items = 10.min(comp.items.len());
    if max_items == 0 {
        return Ok(());
    }
    let box_w = 36u16.min(term_w.saturating_sub(4)).max(20);
    // Place below cursor line when possible
    let cur_screen_row = content_top + (cursor_row.saturating_sub(scroll_row)) as u16;
    let prefix_off = comp.start_col.saturating_sub(scroll_col) as u16;
    let mut left = editor_text_col.saturating_add(prefix_off);
    if left + box_w > term_w {
        left = term_w.saturating_sub(box_w).max(1);
    }
    let mut top = cur_screen_row.saturating_add(1);
    if top + max_items as u16 + 1 >= term_h {
        top = cur_screen_row.saturating_sub(max_items as u16 + 1).max(content_top);
    }

    // header
    term.move_to(top, left)?;
    term.set_bg(45, 45, 48)?;
    term.set_fg(156, 220, 254)?;
    term.write_str(&pad_or_trunc(
        &format!(" suggestions · `{}` ", comp.prefix),
        box_w as usize,
    ))?;

    let scroll_sel = comp.selected.saturating_sub(max_items.saturating_sub(1));
    for vis_i in 0..max_items {
        let idx = scroll_sel + vis_i;
        if idx >= comp.items.len() {
            break;
        }
        let item = &comp.items[idx];
        term.move_to(top + 1 + vis_i as u16, left)?;
        if idx == comp.selected {
            term.set_bg(9, 71, 113)?;
            term.set_fg(255, 255, 255)?;
        } else {
            term.set_bg(37, 37, 38)?;
            term.set_fg(212, 212, 212)?;
        }
        let line = format!(
            " {:4} {:<16} {}",
            item.kind.label(),
            truncate(&item.label, 16),
            truncate(&item.detail, 10)
        );
        term.write_str(&pad_or_trunc(&line, box_w as usize))?;
    }
    term.reset_style()?;
    Ok(())
}

fn draw_overlay(
    term: &mut Terminal,
    overlay: &Overlay,
    w: u16,
    h: u16,
    root: &Path,
) -> io::Result<()> {
    let box_w = (w * 3 / 4).clamp(40, w.saturating_sub(4));
    let left = (w.saturating_sub(box_w)) / 2 + 1;
    let top = 4u16;

    match overlay {
        Overlay::None => {}
        Overlay::QuickOpen {
            query,
            selected,
            results,
            ..
        } => {
            let box_h = (results.len().min(12) as u16 + 4).min(h.saturating_sub(4));
            term.move_to(top, left)?;
            term.set_bg(45, 45, 48)?;
            term.set_fg(255, 255, 255)?;
            term.write_str(&pad_or_trunc(" Quick Open (Ctrl+P) — Esc cancel ", box_w as usize))?;
            term.move_to(top + 1, left)?;
            term.set_fg(86, 156, 214)?;
            term.write_str(&pad_or_trunc(&format!(" > {query}_"), box_w as usize))?;
            for (i, p) in results.iter().take(12).enumerate() {
                term.move_to(top + 2 + i as u16, left)?;
                let rel = p.strip_prefix(root).unwrap_or(p);
                let line = format!("  {}", rel.display());
                if i == *selected {
                    term.set_bg(9, 71, 113)?;
                    term.set_fg(255, 255, 255)?;
                } else {
                    term.set_bg(37, 37, 38)?;
                    term.set_fg(200, 200, 200)?;
                }
                term.write_str(&pad_or_trunc(&line, box_w as usize))?;
            }
            let _ = box_h;
            term.reset_style()?;
        }
        Overlay::Find { query, matches, match_idx, .. } => {
            term.move_to(top, left)?;
            term.set_bg(45, 45, 48)?;
            term.set_fg(255, 255, 255)?;
            let info = if matches.is_empty() {
                "no results".into()
            } else {
                format!("{}/{}", match_idx + 1, matches.len())
            };
            term.write_str(&pad_or_trunc(
                &format!(" Find (Ctrl+F)  [{info}]  Enter=next  Esc=close "),
                box_w as usize,
            ))?;
            term.move_to(top + 1, left)?;
            term.set_fg(206, 145, 120)?;
            term.write_str(&pad_or_trunc(&format!(" Find: {query}_"), box_w as usize))?;
            term.reset_style()?;
        }
        Overlay::Replace {
            find,
            repl,
            field,
            matches,
            match_idx,
            ..
        } => {
            term.move_to(top, left)?;
            term.set_bg(45, 45, 48)?;
            term.set_fg(255, 255, 255)?;
            let info = if matches.is_empty() {
                "0".into()
            } else {
                format!("{}/{}", match_idx + 1, matches.len())
            };
            term.write_str(&pad_or_trunc(
                &format!(" Replace (Ctrl+H) [{info}] Tab=field Enter=one Ctrl+A=all Esc "),
                box_w as usize,
            ))?;
            term.move_to(top + 1, left)?;
            if *field == 0 {
                term.set_fg(255, 255, 100)?;
            } else {
                term.set_fg(200, 200, 200)?;
            }
            term.write_str(&pad_or_trunc(&format!(" Find:    {find}{}", if *field == 0 { "_" } else { "" }), box_w as usize))?;
            term.move_to(top + 2, left)?;
            if *field == 1 {
                term.set_fg(255, 255, 100)?;
            } else {
                term.set_fg(200, 200, 200)?;
            }
            term.write_str(&pad_or_trunc(&format!(" Replace: {repl}{}", if *field == 1 { "_" } else { "" }), box_w as usize))?;
            term.reset_style()?;
        }
    }
    Ok(())
}

fn draw_help(term: &mut Terminal, w: u16, h: u16) -> io::Result<()> {
    let lines = [
        " tui-code — commands (VS Code–like) ",
        "────────────────────────────────────",
        " Ctrl+P     Quick Open any file",
        " Ctrl+F     Find in current file",
        " Ctrl+H     Find and Replace",
        " Ctrl+J     Integrated terminal panel (bottom)",
        "   [+] / ^N  New terminal tab",
        " Ctrl+Space Autocomplete (or type an ident)",
        "   Tab/Enter  accept suggestion",
        "   ↑↓         choose suggestion",
        "   Esc        dismiss suggestions",
        " Ctrl+S     Save",
        " Ctrl+Z     Undo",
        " Ctrl+Y     Redo",
        " Ctrl+C     Quit",
        " Ctrl+B     Explorer ↔ Editor",
        " Esc        Explorer / close overlay",
        " Mouse      Click files / move cursor",
        "",
        " Esc closes this help ",
    ];
    let box_w = 48u16.min(w.saturating_sub(4)).max(30);
    let box_h = lines.len() as u16;
    let top = (h.saturating_sub(box_h)) / 2 + 1;
    let left = (w.saturating_sub(box_w)) / 2 + 1;
    for (i, line) in lines.iter().enumerate() {
        term.move_to(top + i as u16, left)?;
        term.set_bg(45, 45, 48)?;
        term.set_fg(220, 220, 220)?;
        term.write_str(&pad_or_trunc(line, box_w as usize))?;
        term.reset_style()?;
    }
    Ok(())
}

fn line_num_width(n_lines: usize) -> usize {
    let n = n_lines.max(1);
    let mut w = 1usize;
    let mut v = 10usize;
    while v <= n {
        w += 1;
        v = v.saturating_mul(10);
        if w > 6 {
            break;
        }
    }
    w.max(3)
}

fn pad_or_trunc(s: &str, width: usize) -> String {
    let width = width.min(10_000); // guard against nonsense
    let chars: Vec<char> = s.chars().collect();
    if chars.len() >= width {
        chars.into_iter().take(width).collect()
    } else {
        let mut out: String = chars.into_iter().collect();
        let cur = out.chars().count();
        if cur < width {
            out.push_str(&spaces(width - cur));
        }
        out
    }
}

fn truncate(s: &str, width: usize) -> String {
    let width = width.min(10_000);
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= width {
        return s.to_string();
    }
    if width <= 1 {
        return "…".to_string();
    }
    let mut out: String = chars.into_iter().take(width.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn display_width(s: &str) -> usize {
    s.chars().count()
}
