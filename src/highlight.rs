//! Tree-sitter AST syntax highlighting with incremental reparse.
//! Colors tokens by node kind; only walks nodes intersecting the visible range.

use std::path::Path;

use tree_sitter::{InputEdit, Language, Parser, Point, Tree, TreeCursor};

use crate::theme::Theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Rust,
    Python,
    JavaScript,
}

impl Lang {
    pub fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "rs" => Some(Lang::Rust),
            "py" | "pyi" => Some(Lang::Python),
            "js" | "mjs" | "cjs" | "jsx" => Some(Lang::JavaScript),
            _ => None,
        }
    }

    fn language(self) -> Language {
        match self {
            Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
            Lang::Python => tree_sitter_python::LANGUAGE.into(),
            Lang::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Lang::Rust => "rust",
            Lang::Python => "python",
            Lang::JavaScript => "javascript",
        }
    }
}

/// One colored run on a line (byte offsets within the full source line, including no newline).
#[derive(Clone, Debug)]
pub struct Span {
    pub start_byte: usize,
    pub end_byte: usize,
    pub fg: (u8, u8, u8),
}

/// Incremental highlighter: keeps source + AST; edits use tree-sitter InputEdit.
pub struct Highlighter {
    pub lang: Option<Lang>,
    parser: Parser,
    tree: Option<Tree>,
    /// Full file text with `\n` line endings (no trailing requirement).
    source: String,
    /// Line start byte offsets; last entry is source.len() sentinel optional.
    line_starts: Vec<usize>,
}

impl Highlighter {
    pub fn none() -> Self {
        Self {
            lang: None,
            parser: Parser::new(),
            tree: None,
            source: String::new(),
            line_starts: vec![0],
        }
    }

    pub fn from_lines(lines: &[String], path: Option<&Path>) -> Self {
        let mut h = Self::none();
        h.source = join_lines(lines);
        h.rebuild_line_starts();
        if let Some(p) = path {
            if let Some(lang) = Lang::from_path(p) {
                h.set_language(lang);
                h.full_parse();
            }
        }
        h
    }

    pub fn set_language(&mut self, lang: Lang) {
        self.lang = Some(lang);
        let _ = self.parser.set_language(&lang.language());
    }

    pub fn full_parse(&mut self) {
        if self.lang.is_none() {
            self.tree = None;
            return;
        }
        self.tree = self.parser.parse(&self.source, None);
    }

    fn rebuild_line_starts(&mut self) {
        self.line_starts.clear();
        self.line_starts.push(0);
        for (i, b) in self.source.bytes().enumerate() {
            if b == b'\n' {
                self.line_starts.push(i + 1);
            }
        }
    }

    pub fn line_count(&self) -> usize {
        if self.source.is_empty() {
            return 1;
        }
        let mut n = self.line_starts.len();
        // if source doesn't end with \n, last line is still valid; line_starts has start of each line
        // number of lines = count of \n + 1 unless empty
        if self.source.ends_with('\n') {
            // trailing empty line represented by final line_start at len
            n
        } else {
            n
        }
    }

    fn point_at_byte(&self, byte: usize) -> Point {
        // binary search line
        let row = match self.line_starts.binary_search(&byte) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        };
        let row = row.min(self.line_starts.len().saturating_sub(1));
        let col = byte.saturating_sub(self.line_starts[row]);
        Point {
            row,
            column: col,
        }
    }

    /// Apply an edit described in buffer coordinates (row + char column), then incremental parse.
    pub fn apply_edit(
        &mut self,
        start_row: usize,
        start_col_char: usize,
        old_end_row: usize,
        old_end_col_char: usize,
        new_text: &str,
        new_lines_snapshot: &[String],
    ) {
        if self.lang.is_none() {
            // still keep source in sync for future
            self.source = join_lines(new_lines_snapshot);
            self.rebuild_line_starts();
            return;
        }

        let start_byte = self.char_pos_to_byte(start_row, start_col_char);
        let old_end_byte = self.char_pos_to_byte(old_end_row, old_end_col_char);
        let start_position = self.point_at_byte(start_byte);
        let old_end_position = self.point_at_byte(old_end_byte);

        let new_end_byte = start_byte + new_text.len();
        // compute new_end_position from new_text
        let mut new_end_position = start_position;
        for (i, line) in new_text.split('\n').enumerate() {
            if i == 0 {
                new_end_position.column = start_position.column + line.len();
            } else {
                new_end_position.row = start_position.row + i;
                new_end_position.column = line.len();
            }
        }

        let edit = InputEdit {
            start_byte,
            old_end_byte,
            new_end_byte,
            start_position,
            old_end_position,
            new_end_position,
        };

        if let Some(tree) = self.tree.as_mut() {
            tree.edit(&edit);
        }

        // sync source from buffer lines (authoritative)
        self.source = join_lines(new_lines_snapshot);
        self.rebuild_line_starts();

        let old_tree = self.tree.take();
        self.tree = self.parser.parse(&self.source, old_tree.as_ref());
    }

    /// Full resync (e.g. after load). Prefer apply_edit while typing.
    pub fn resync_from_lines(&mut self, lines: &[String], path: Option<&Path>) {
        self.source = join_lines(lines);
        self.rebuild_line_starts();
        match path.and_then(Lang::from_path) {
            Some(lang) => {
                if self.lang != Some(lang) {
                    self.set_language(lang);
                }
                self.full_parse();
            }
            None => {
                self.lang = None;
                self.tree = None;
            }
        }
    }

    fn char_pos_to_byte(&self, row: usize, col_char: usize) -> usize {
        let line_start = *self
            .line_starts
            .get(row)
            .unwrap_or(self.line_starts.last().unwrap_or(&0));
        let line_end = if row + 1 < self.line_starts.len() {
            // exclude the newline at end of line content for char indexing on line
            let next = self.line_starts[row + 1];
            next.saturating_sub(1) // point at \n
        } else {
            self.source.len()
        };
        let line = if line_start <= line_end && line_end <= self.source.len() {
            // if ends with \n, line content is [line_start, line_end) where line_end is \n index
            let end = if line_end < self.source.len() && self.source.as_bytes().get(line_end) == Some(&b'\n')
            {
                line_end
            } else if row + 1 < self.line_starts.len() {
                self.line_starts[row + 1].saturating_sub(1)
            } else {
                self.source.len()
            };
            // simpler: take until newline or eof
            let slice = &self.source[line_start.min(self.source.len())..];
            let content_end = slice.find('\n').map(|i| line_start + i).unwrap_or(self.source.len());
            &self.source[line_start.min(self.source.len())..content_end.min(self.source.len())]
        } else {
            ""
        };
        let _ = line_end;
        let byte_in_line = line
            .char_indices()
            .nth(col_char)
            .map(|(i, _)| i)
            .unwrap_or(line.len());
        line_start + byte_in_line
    }

    fn line_byte_range(&self, row: usize) -> (usize, usize) {
        let start = *self.line_starts.get(row).unwrap_or(&self.source.len());
        let end = if row + 1 < self.line_starts.len() {
            // exclude newline
            self.line_starts[row + 1].saturating_sub(1)
        } else {
            self.source.len()
        };
        let end = end.min(self.source.len()).max(start);
        // if end points at \n already subtracted; if no newline at eof, end is len
        let end = if end < self.source.len() && self.source.as_bytes()[end] == b'\n' {
            end
        } else {
            end
        };
        (start, end)
    }

    /// Spans for a single source line (byte offsets relative to line start).
    /// Only traverses AST nodes that intersect this line — not the whole file.
    /// Borrow the current AST (for autocomplete symbol collection).
    pub fn tree_ref(&self) -> Option<&tree_sitter::Tree> {
        self.tree.as_ref()
    }

    /// Full source text used by the parser.
    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn spans_for_line(&self, row: usize) -> Vec<Span> {
        let Some(tree) = self.tree.as_ref() else {
            return Vec::new();
        };
        let (line_start, line_end) = self.line_byte_range(row);
        if line_start >= line_end && line_start >= self.source.len() {
            return Vec::new();
        }
        let mut spans = Vec::new();
        let root = tree.root_node();
        let mut cursor = root.walk();
        collect_spans(
            &mut cursor,
            &self.source,
            line_start,
            line_end,
            &mut spans,
        );
        spans.sort_by_key(|s| s.start_byte);
        // merge / ensure coverage gaps use default fg when rendering
        spans
    }

    /// Visible window only: rows in [start_row, end_row).
    pub fn spans_visible(&self, start_row: usize, end_row: usize) -> Vec<Vec<Span>> {
        (start_row..end_row)
            .map(|r| self.spans_for_line(r))
            .collect()
    }
}

fn join_lines(lines: &[String]) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let mut s = lines.join("\n");
    // match editor save convention: we store without forcing trailing newline in buffer lines
    // but tree-sitter is fine either way; keep exact join
    let _ = &mut s;
    s
}

/// Walk AST iteratively; emit colored spans for leaves intersecting [range_start, range_end).
/// Iterative (not recursive) so a bad tree cannot blow the stack or hang the TUI.
fn collect_spans(
    cursor: &mut TreeCursor,
    _source: &str,
    range_start: usize,
    range_end: usize,
    out: &mut Vec<Span>,
) {
    // Limit work per line so the UI never freezes on a huge AST region.
    const MAX_NODES: usize = 50_000;
    let mut visited = 0usize;
    let mut done = false;

    // Ensure we start from the node under the cursor (caller sets root.walk()).
    'outer: while !done && visited < MAX_NODES {
        visited += 1;
        let node = cursor.node();
        let ns = node.start_byte();
        let ne = node.end_byte();

        let intersects = ne > range_start && ns < range_end;

        if intersects && cursor.goto_first_child() {
            // descend
            continue;
        }

        if intersects {
            let clip_s = ns.max(range_start);
            let clip_e = ne.min(range_end);
            if clip_s < clip_e {
                let kind = node.kind();
                let fg = color_for_kind(kind, node.is_named());
                out.push(Span {
                    start_byte: clip_s - range_start,
                    end_byte: clip_e - range_start,
                    fg,
                });
            }
        }

        // advance: next sibling, or up + next sibling
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                done = true;
                break 'outer;
            }
        }
    }
}

/// Map tree-sitter node kind → VS Code Dark+ inspired RGB.
pub fn color_for_kind(kind: &str, named: bool) -> (u8, u8, u8) {
    // punctuation / operators (often anonymous)
    match kind {
        "(" | ")" | "[" | "]" | "{" | "}" | "," | ";" | "." | "::" | ":" | "->" | "=>" | "="
        | "==" | "!=" | "<" | ">" | "<=" | ">=" | "+" | "-" | "*" | "/" | "%" | "&" | "|" | "^"
        | "!" | "?" | "..." | "+=" | "-=" | "*=" | "/=" | "&&" | "||" | "<<" | ">>" | ".."
        | "..=" | "@" | "#" | "$" | "\\" => return Theme::PUNCTUATION,
        "\"" | "'" | "`" => return Theme::STRING,
        _ => {}
    }

    // comments
    if kind.contains("comment") {
        return Theme::COMMENT;
    }

    // strings / chars / regex
    if kind.contains("string")
        || kind.contains("char")
        || kind == "escape_sequence"
        || kind.contains("template")
        || kind == "raw_string_literal"
        || kind == "string_content"
        || kind == "string_fragment"
        || kind == "interpolation"
    {
        return Theme::STRING;
    }

    // numbers
    if kind.contains("integer")
        || kind.contains("float")
        || kind.contains("number")
        || kind == "int_literal"
        || kind == "float_literal"
        || kind == "boolean"
        || kind == "true"
        || kind == "false"
        || kind == "none"
        || kind == "null"
        || kind == "undefined"
        || kind == "nil"
    {
        return Theme::NUMBER;
    }

    // types
    if kind == "type_identifier"
        || kind == "primitive_type"
        || kind == "type"
        || kind == "class_name"
        || kind.contains("type") && named
    {
        return Theme::TYPE;
    }

    // attributes / decorators
    if kind.contains("attribute") || kind.contains("decorator") || kind == "meta" {
        return Theme::ATTRIBUTE;
    }

    // fields / properties
    if kind == "field_identifier" || kind == "property_identifier" || kind == "shorthand_property_identifier" {
        return Theme::FIELD;
    }

    // function-ish identifiers often still "identifier" — keyword list below handles keywords;
    // constructors
    if kind == "constructor" {
        return Theme::FUNCTION;
    }

    // keywords (named keyword nodes or anonymous keyword text)
    if is_keyword(kind) {
        return Theme::KEYWORD;
    }

    // identifiers
    if kind == "identifier" || kind == "variable_name" || kind == "constant" {
        return Theme::IDENTIFIER;
    }

    if kind == "self" || kind == "super" || kind == "this" || kind == "cls" {
        return Theme::KEYWORD;
    }

    // default
    if named {
        Theme::EDITOR_FG
    } else {
        // anonymous often operators/keywords as text
        if is_keyword(kind) {
            Theme::KEYWORD
        } else {
            Theme::PUNCTUATION
        }
    }
}

fn is_keyword(kind: &str) -> bool {
    if kind.ends_with("_keyword")
        || matches!(
            kind,
            "keyword" | "visibility_modifier" | "mutable_specifier" | "function" | "variable"
        )
    {
        return true;
    }
    // Shared + language-specific keyword tokens (deduped for match exhaustiveness).
    matches!(
        kind,
        "use" | "fn" | "let" | "mut" | "pub" | "struct" | "enum" | "impl" | "trait" | "for"
            | "in" | "if" | "else" | "match" | "return" | "while" | "loop" | "break" | "continue"
            | "const" | "static" | "mod" | "crate" | "super" | "self" | "Self" | "as" | "where"
            | "async" | "await" | "move" | "ref" | "type" | "where_clause" | "dyn" | "unsafe"
            | "extern" | "box" | "yield" | "union" | "true" | "false" | "def" | "class" | "import"
            | "from" | "with" | "lambda" | "pass" | "raise" | "try" | "except" | "finally"
            | "global" | "nonlocal" | "assert" | "del" | "and" | "or" | "not" | "is" | "None"
            | "True" | "False" | "var" | "function" | "do" | "switch" | "case" | "default" | "new"
            | "this" | "typeof" | "instanceof" | "of" | "extends" | "get" | "set" | "export"
            | "void" | "delete" | "throw" | "catch" | "debugger" | "null" | "undefined"
    )
}
