//! Lexically scoped autocomplete (VS Code–style) via tree-sitter AST.
//! Only names visible at the cursor — not every identifier in the file.

use std::collections::BTreeMap;

use tree_sitter::Node;

use crate::highlight::{Highlighter, Lang};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionKind {
    Keyword,
    Function,
    Variable,
    Type,
    Field,
    Module,
    Snippet,
}

impl CompletionKind {
    pub fn label(self) -> &'static str {
        match self {
            CompletionKind::Keyword => "kw",
            CompletionKind::Function => "fn",
            CompletionKind::Variable => "var",
            CompletionKind::Type => "type",
            CompletionKind::Field => "field",
            CompletionKind::Module => "mod",
            CompletionKind::Snippet => "snip",
        }
    }
}

#[derive(Clone, Debug)]
pub struct CompletionItem {
    pub label: String,
    pub kind: CompletionKind,
    pub insert: String,
    pub detail: String,
}

#[derive(Clone, Debug)]
pub struct CompletionState {
    pub prefix: String,
    pub start_col: usize,
    pub items: Vec<CompletionItem>,
    pub selected: usize,
}

impl CompletionState {
    pub fn selected_item(&self) -> Option<&CompletionItem> {
        self.items.get(self.selected)
    }
}

pub fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

pub fn prefix_at(line: &str, cursor_col: usize) -> (usize, String) {
    let chars: Vec<char> = line.chars().collect();
    let end = cursor_col.min(chars.len());
    let mut start = end;
    while start > 0 && is_ident_char(chars[start - 1]) {
        start -= 1;
    }
    let prefix: String = chars[start..end].iter().collect();
    (start, prefix)
}

fn char_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

/// Byte offset of (row, char_col) in `lines` joined with `\n` (matches highlighter source).
pub fn cursor_byte_offset(lines: &[String], row: usize, col: usize) -> usize {
    let mut off = 0usize;
    for (i, line) in lines.iter().enumerate() {
        if i >= row {
            return off + char_to_byte(line, col);
        }
        off += line.len() + 1; // + '\n'
    }
    off
}

pub fn keywords_for(lang: Option<Lang>) -> &'static [&'static str] {
    match lang {
        Some(Lang::Rust) => &[
            "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
            "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod",
            "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super",
            "trait", "true", "type", "unsafe", "use", "where", "while", "yield", "union", "box",
            "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize",
            "f32", "f64", "bool", "char", "str", "String", "Vec", "Option", "Result", "Some", "None",
            "Ok", "Err", "derive", "Debug", "Clone", "Copy", "Default", "PartialEq", "Eq", "Hash",
            "println", "eprintln", "format", "vec", "panic", "todo", "unimplemented", "assert",
            "assert_eq", "dbg",
        ],
        Some(Lang::Python) => &[
            "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class",
            "continue", "def", "del", "elif", "else", "except", "finally", "for", "from", "global",
            "if", "import", "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return",
            "try", "while", "with", "yield", "self", "cls", "print", "len", "range", "str", "int",
            "float", "list", "dict", "set", "tuple", "bool", "super", "property", "staticmethod",
            "classmethod", "isinstance", "enumerate", "zip", "open", "Exception",
        ],
        Some(Lang::JavaScript) => &[
            "await", "break", "case", "catch", "class", "const", "continue", "debugger", "default",
            "delete", "do", "else", "export", "extends", "false", "finally", "for", "function",
            "if", "import", "in", "instanceof", "let", "new", "null", "return", "static", "super",
            "switch", "this", "throw", "true", "try", "typeof", "var", "void", "while", "with",
            "yield", "async", "of", "from", "as", "console", "log", "error", "warn", "JSON",
            "Object", "Array", "String", "Number", "Boolean", "Promise", "Map", "Set", "undefined",
            "document", "window", "Math", "Date", "Error",
        ],
        None => &["if", "else", "for", "while", "return", "function", "const", "let", "var", "true", "false", "null"],
    }
}

/// One in-scope symbol.
#[derive(Clone, Debug)]
struct Symbol {
    name: String,
    kind: CompletionKind,
    detail: &'static str,
}

/// Collect symbols visible at `cursor_byte` for the given language AST.
fn scoped_symbols(lang: Lang, root: Node, source: &str, cursor_byte: usize) -> Vec<Symbol> {
    let mut map: BTreeMap<String, Symbol> = BTreeMap::new();

    // Innermost node containing the cursor
    let mut node = root.descendant_for_byte_range(cursor_byte, cursor_byte);
    // Walk up collecting scopes
    let mut scopes: Vec<Node> = Vec::new();
    while let Some(n) = node {
        if is_scope_node(lang, n.kind()) {
            scopes.push(n);
        }
        node = n.parent();
    }
    // Always include root as outermost scope
    if scopes.last().map(|n| n.id()) != Some(root.id()) {
        scopes.push(root);
    }

    // Outermost → innermost so inner names override
    for scope in scopes.iter().rev() {
        collect_decls_in_scope(lang, *scope, source, cursor_byte, &mut map);
    }

    // Language builtins always "in scope"
    for (name, kind, detail) in builtins(lang) {
        map.entry(name.to_string()).or_insert(Symbol {
            name: name.to_string(),
            kind,
            detail,
        });
    }

    map.into_values().collect()
}

fn is_scope_node(lang: Lang, kind: &str) -> bool {
    match lang {
        Lang::Rust => matches!(
            kind,
            "source_file"
                | "function_item"
                | "impl_item"
                | "mod_item"
                | "block"
                | "closure_expression"
                | "for_expression"
                | "while_expression"
                | "loop_expression"
                | "if_expression"
                | "match_expression"
                | "match_arm"
        ),
        Lang::Python => matches!(
            kind,
            "module"
                | "function_definition"
                | "class_definition"
                | "block"
                | "for_statement"
                | "while_statement"
                | "with_statement"
                | "if_statement"
                | "try_statement"
                | "lambda"
                | "comprehension"
                | "list_comprehension"
                | "dictionary_comprehension"
                | "set_comprehension"
                | "generator_expression"
        ),
        Lang::JavaScript => matches!(
            kind,
            "program"
                | "function_declaration"
                | "function"
                | "generator_function_declaration"
                | "arrow_function"
                | "method_definition"
                | "class_declaration"
                | "class_body"
                | "statement_block"
                | "for_statement"
                | "for_in_statement"
                | "for_of_statement"
                | "while_statement"
                | "do_statement"
                | "if_statement"
                | "try_statement"
                | "catch_clause"
                | "switch_statement"
        ),
    }
}

fn collect_decls_in_scope(
    lang: Lang,
    scope: Node,
    source: &str,
    cursor_byte: usize,
    map: &mut BTreeMap<String, Symbol>,
) {
    match lang {
        Lang::Rust => collect_rust(scope, source, cursor_byte, map),
        Lang::Python => collect_python(scope, source, cursor_byte, map),
        Lang::JavaScript => collect_js(scope, source, cursor_byte, map),
    }
}

fn insert_sym(
    map: &mut BTreeMap<String, Symbol>,
    name: &str,
    kind: CompletionKind,
    detail: &'static str,
) {
    if name.is_empty() || !name.chars().all(is_ident_char) {
        return;
    }
    if name.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(true) {
        return;
    }
    map.insert(
        name.to_string(),
        Symbol {
            name: name.to_string(),
            kind,
            detail,
        },
    );
}

fn node_text<'a>(source: &'a str, node: Node) -> &'a str {
    let s = node.start_byte().min(source.len());
    let e = node.end_byte().min(source.len());
    if s >= e {
        return "";
    }
    source.get(s..e).unwrap_or("")
}

fn child_by_field<'a>(node: Node<'a>, field: &str) -> Option<Node<'a>> {
    node.child_by_field_name(field)
}

/// Recursively collect pattern bindings (Rust / general identifier leaves in patterns).
fn collect_pattern_idents(node: Node, source: &str, map: &mut BTreeMap<String, Symbol>, kind: CompletionKind, detail: &'static str) {
    let k = node.kind();
    if matches!(
        k,
        "identifier" | "variable_name" | "shorthand_property_identifier"
    ) {
        let t = node_text(source, node);
        insert_sym(map, t, kind, detail);
        return;
    }
    // Don't descend into types/calls inside patterns unnecessarily — still walk children
    let mut c = node.walk();
    for child in node.children(&mut c) {
        if child.is_named() {
            collect_pattern_idents(child, source, map, kind, detail);
        }
    }
}

// ─── Rust ───────────────────────────────────────────────────────────

fn collect_rust(scope: Node, source: &str, cursor_byte: usize, map: &mut BTreeMap<String, Symbol>) {
    let sk = scope.kind();
    let mut walk = scope.walk();
    for child in scope.named_children(&mut walk) {
        let ck = child.kind();
        // Module / impl / source_file level items — always visible in this scope
        match ck {
            "function_item" => {
                if let Some(name) = child_by_field(child, "name") {
                    insert_sym(map, node_text(source, name), CompletionKind::Function, "function");
                }
                // parameters only if cursor is inside this function body
                if cursor_byte >= child.start_byte() && cursor_byte <= child.end_byte() {
                    if let Some(params) = child_by_field(child, "parameters") {
                        collect_rust_params(params, source, map);
                    }
                }
            }
            "struct_item" | "enum_item" | "union_item" | "type_item" | "trait_item" => {
                if let Some(name) = child_by_field(child, "name") {
                    insert_sym(map, node_text(source, name), CompletionKind::Type, "type");
                }
            }
            "const_item" | "static_item" => {
                if let Some(name) = child_by_field(child, "name") {
                    insert_sym(map, node_text(source, name), CompletionKind::Variable, "const");
                }
            }
            "mod_item" => {
                if let Some(name) = child_by_field(child, "name") {
                    insert_sym(map, node_text(source, name), CompletionKind::Module, "module");
                }
            }
            "use_declaration" => {
                // use foo::Bar — collect last identifier segments (best effort)
                collect_use_idents(child, source, map);
            }
            "let_declaration" => {
                // only if declaration fully before cursor (not visible before init in Rust strictly,
                // but we use start_byte < cursor so current incomplete let name still works)
                if child.start_byte() < cursor_byte {
                    if let Some(pat) = child_by_field(child, "pattern") {
                        collect_pattern_idents(pat, source, map, CompletionKind::Variable, "local");
                    }
                }
            }
            "const_parameter" | "type_parameter" => {
                if let Some(name) = child_by_field(child, "name") {
                    insert_sym(map, node_text(source, name), CompletionKind::Type, "type param");
                } else {
                    // some grammars use identifier child
                    let mut w = child.walk();
                    for ch in child.named_children(&mut w) {
                        if ch.kind() == "type_identifier" || ch.kind() == "identifier" {
                            insert_sym(map, node_text(source, ch), CompletionKind::Type, "type param");
                        }
                    }
                }
            }
            "for_expression" => {
                if cursor_byte >= child.start_byte() && cursor_byte <= child.end_byte() {
                    if let Some(pat) = child_by_field(child, "pattern") {
                        collect_pattern_idents(pat, source, map, CompletionKind::Variable, "for binding");
                    }
                }
            }
            "closure_expression" => {
                if cursor_byte >= child.start_byte() && cursor_byte <= child.end_byte() {
                    if let Some(params) = child_by_field(child, "parameters") {
                        collect_rust_params(params, source, map);
                    }
                }
            }
            "match_arm" => {
                if cursor_byte >= child.start_byte() && cursor_byte <= child.end_byte() {
                    if let Some(pat) = child_by_field(child, "pattern") {
                        collect_pattern_idents(pat, source, map, CompletionKind::Variable, "match binding");
                    }
                }
            }
            "if_expression" | "while_expression" => {
                // if let / while let
                if cursor_byte >= child.start_byte() && cursor_byte <= child.end_byte() {
                    let mut w = child.walk();
                    for ch in child.named_children(&mut w) {
                        if ch.kind() == "let_condition" || ch.kind().contains("condition") {
                            collect_pattern_idents(ch, source, map, CompletionKind::Variable, "binding");
                        }
                    }
                }
            }
            "impl_item" => {
                // methods inside impl when cursor in impl
                if cursor_byte >= child.start_byte() && cursor_byte <= child.end_byte() {
                    let mut w = child.walk();
                    for ch in child.named_children(&mut w) {
                        if ch.kind() == "function_item" {
                            if let Some(name) = child_by_field(ch, "name") {
                                insert_sym(map, node_text(source, name), CompletionKind::Function, "method");
                            }
                            if cursor_byte >= ch.start_byte() && cursor_byte <= ch.end_byte() {
                                if let Some(params) = child_by_field(ch, "parameters") {
                                    collect_rust_params(params, source, map);
                                }
                            }
                        }
                    }
                }
            }
            "block" if sk == "block" || sk == "function_item" || sk == "source_file" => {
                // nested blocks handled as separate scopes when walking parents
            }
            _ => {}
        }
    }

    // Parameters when scope itself is a function
    if sk == "function_item" || sk == "closure_expression" {
        if let Some(params) = child_by_field(scope, "parameters") {
            collect_rust_params(params, source, map);
        }
    }
}

fn collect_rust_params(params: Node, source: &str, map: &mut BTreeMap<String, Symbol>) {
    let mut w = params.walk();
    for ch in params.named_children(&mut w) {
        if ch.kind() == "parameter" || ch.kind() == "self_parameter" {
            if ch.kind() == "self_parameter" {
                insert_sym(map, "self", CompletionKind::Variable, "self");
                continue;
            }
            if let Some(pat) = child_by_field(ch, "pattern") {
                collect_pattern_idents(pat, source, map, CompletionKind::Variable, "param");
            } else {
                // fallback identifier
                let mut w2 = ch.walk();
                for id in ch.named_children(&mut w2) {
                    if id.kind() == "identifier" {
                        insert_sym(map, node_text(source, id), CompletionKind::Variable, "param");
                    }
                }
            }
        }
    }
}

fn collect_use_idents(node: Node, source: &str, map: &mut BTreeMap<String, Symbol>) {
    let mut w = node.walk();
    for ch in node.named_children(&mut w) {
        match ch.kind() {
            "identifier" | "type_identifier" | "scoped_identifier" | "use_as_clause" | "use_list" | "use_wildcard" => {
                if ch.kind() == "identifier" || ch.kind() == "type_identifier" {
                    let t = node_text(source, ch);
                    let kind = if t.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
                        CompletionKind::Type
                    } else {
                        CompletionKind::Module
                    };
                    insert_sym(map, t, kind, "import");
                } else {
                    collect_use_idents(ch, source, map);
                }
            }
            _ if ch.is_named() => collect_use_idents(ch, source, map),
            _ => {}
        }
    }
}

// ─── Python ─────────────────────────────────────────────────────────

fn collect_python(scope: Node, source: &str, cursor_byte: usize, map: &mut BTreeMap<String, Symbol>) {
    let sk = scope.kind();
    let mut walk = scope.walk();
    for child in scope.named_children(&mut walk) {
        let ck = child.kind();
        match ck {
            "function_definition" => {
                if let Some(name) = child_by_field(child, "name") {
                    insert_sym(map, node_text(source, name), CompletionKind::Function, "function");
                }
                if cursor_byte >= child.start_byte() && cursor_byte <= child.end_byte() {
                    if let Some(params) = child_by_field(child, "parameters") {
                        collect_python_params(params, source, map);
                    }
                }
            }
            "class_definition" => {
                if let Some(name) = child_by_field(child, "name") {
                    insert_sym(map, node_text(source, name), CompletionKind::Type, "class");
                }
            }
            "import_statement" | "import_from_statement" => {
                collect_python_import(child, source, map);
            }
            "expression_statement" => {
                // assignment as statement
                if child.start_byte() < cursor_byte {
                    let mut w = child.walk();
                    for ch in child.named_children(&mut w) {
                        if ch.kind() == "assignment" || ch.kind() == "augmented_assignment" {
                            if let Some(left) = child_by_field(ch, "left") {
                                collect_pattern_idents(left, source, map, CompletionKind::Variable, "local");
                            }
                        }
                    }
                }
            }
            "assignment" | "augmented_assignment" => {
                if child.start_byte() < cursor_byte {
                    if let Some(left) = child_by_field(child, "left") {
                        collect_pattern_idents(left, source, map, CompletionKind::Variable, "local");
                    }
                }
            }
            "for_statement" => {
                if cursor_byte >= child.start_byte() && cursor_byte <= child.end_byte() {
                    if let Some(left) = child_by_field(child, "left") {
                        collect_pattern_idents(left, source, map, CompletionKind::Variable, "for binding");
                    }
                }
            }
            "with_statement" => {
                if cursor_byte >= child.start_byte() && cursor_byte <= child.end_byte() {
                    let mut w = child.walk();
                    for ch in child.named_children(&mut w) {
                        if ch.kind() == "with_item" {
                            if let Some(alias) = child_by_field(ch, "alias").or_else(|| child_by_field(ch, "name")) {
                                collect_pattern_idents(alias, source, map, CompletionKind::Variable, "with");
                            }
                        }
                    }
                }
            }
            "except_clause" => {
                if cursor_byte >= child.start_byte() && cursor_byte <= child.end_byte() {
                    let mut w = child.walk();
                    for ch in child.named_children(&mut w) {
                        if ch.kind() == "as_pattern" || ch.kind() == "identifier" {
                            collect_pattern_idents(ch, source, map, CompletionKind::Variable, "except");
                        }
                    }
                }
            }
            "lambda" => {
                if cursor_byte >= child.start_byte() && cursor_byte <= child.end_byte() {
                    if let Some(params) = child_by_field(child, "parameters") {
                        collect_python_params(params, source, map);
                    }
                }
            }
            _ => {}
        }
    }

    if sk == "function_definition" || sk == "lambda" {
        if let Some(params) = child_by_field(scope, "parameters") {
            collect_python_params(params, source, map);
        }
        if sk == "function_definition" {
            insert_sym(map, "self", CompletionKind::Variable, "self");
        }
    }
    if sk == "class_definition" {
        // methods already collected as function_definition children
    }
}

fn collect_python_params(params: Node, source: &str, map: &mut BTreeMap<String, Symbol>) {
    let mut w = params.walk();
    for ch in params.named_children(&mut w) {
        match ch.kind() {
            "identifier" | "typed_parameter" | "default_parameter" | "typed_default_parameter"
            | "list_splat_pattern" | "dictionary_splat_pattern" => {
                collect_pattern_idents(ch, source, map, CompletionKind::Variable, "param");
            }
            _ => {
                if ch.is_named() {
                    collect_pattern_idents(ch, source, map, CompletionKind::Variable, "param");
                }
            }
        }
    }
}

fn collect_python_import(node: Node, source: &str, map: &mut BTreeMap<String, Symbol>) {
    let mut w = node.walk();
    for ch in node.named_children(&mut w) {
        match ch.kind() {
            "dotted_name" | "aliased_import" | "identifier" | "relative_import" => {
                if ch.kind() == "identifier" {
                    insert_sym(map, node_text(source, ch), CompletionKind::Module, "import");
                } else if ch.kind() == "aliased_import" {
                    if let Some(alias) = child_by_field(ch, "alias").or_else(|| {
                        // last identifier
                        let mut w2 = ch.walk();
                        ch.named_children(&mut w2).last()
                    }) {
                        insert_sym(map, node_text(source, alias), CompletionKind::Module, "import");
                    }
                } else {
                    // dotted_name — use last segment
                    let mut w2 = ch.walk();
                    let mut last = None;
                    for seg in ch.named_children(&mut w2) {
                        if seg.kind() == "identifier" {
                            last = Some(seg);
                        }
                    }
                    if let Some(seg) = last {
                        insert_sym(map, node_text(source, seg), CompletionKind::Module, "import");
                    }
                }
            }
            _ if ch.is_named() => collect_python_import(ch, source, map),
            _ => {}
        }
    }
}

// ─── JavaScript ─────────────────────────────────────────────────────

fn collect_js(scope: Node, source: &str, cursor_byte: usize, map: &mut BTreeMap<String, Symbol>) {
    let sk = scope.kind();
    let mut walk = scope.walk();
    for child in scope.named_children(&mut walk) {
        let ck = child.kind();
        match ck {
            "function_declaration" | "generator_function_declaration" => {
                if let Some(name) = child_by_field(child, "name") {
                    insert_sym(map, node_text(source, name), CompletionKind::Function, "function");
                }
                // hoisted: always in scope for containing function/program
                if cursor_byte >= child.start_byte() && cursor_byte <= child.end_byte() {
                    if let Some(params) = child_by_field(child, "parameters") {
                        collect_js_params(params, source, map);
                    }
                }
            }
            "lexical_declaration" | "variable_declaration" => {
                // let/const: only if before cursor (TDZ simplified as start < cursor)
                // var: treat same for simplicity
                if child.start_byte() < cursor_byte {
                    let mut w = child.walk();
                    for ch in child.named_children(&mut w) {
                        if ch.kind() == "variable_declarator" {
                            if let Some(name) = child_by_field(ch, "name") {
                                collect_pattern_idents(name, source, map, CompletionKind::Variable, "local");
                            }
                        }
                    }
                }
            }
            "class_declaration" => {
                if let Some(name) = child_by_field(child, "name") {
                    insert_sym(map, node_text(source, name), CompletionKind::Type, "class");
                }
            }
            "import_statement" => {
                collect_js_import(child, source, map);
            }
            "for_statement" | "for_in_statement" | "for_of_statement" => {
                if cursor_byte >= child.start_byte() && cursor_byte <= child.end_byte() {
                    let mut w = child.walk();
                    for ch in child.named_children(&mut w) {
                        if matches!(ch.kind(), "lexical_declaration" | "variable_declaration" | "identifier" | "object_pattern" | "array_pattern") {
                            collect_pattern_idents(ch, source, map, CompletionKind::Variable, "for binding");
                        }
                    }
                }
            }
            "arrow_function" | "function" => {
                if cursor_byte >= child.start_byte() && cursor_byte <= child.end_byte() {
                    if let Some(params) = child_by_field(child, "parameters") {
                        collect_js_params(params, source, map);
                    }
                }
            }
            "method_definition" => {
                if let Some(name) = child_by_field(child, "name") {
                    insert_sym(map, node_text(source, name), CompletionKind::Function, "method");
                }
                if cursor_byte >= child.start_byte() && cursor_byte <= child.end_byte() {
                    if let Some(params) = child_by_field(child, "parameters") {
                        collect_js_params(params, source, map);
                    }
                }
            }
            "catch_clause" => {
                if cursor_byte >= child.start_byte() && cursor_byte <= child.end_byte() {
                    if let Some(param) = child_by_field(child, "parameter") {
                        collect_pattern_idents(param, source, map, CompletionKind::Variable, "catch");
                    }
                }
            }
            _ => {}
        }
    }

    if matches!(sk, "function_declaration" | "arrow_function" | "function" | "method_definition") {
        if let Some(params) = child_by_field(scope, "parameters") {
            collect_js_params(params, source, map);
        }
    }
    if sk == "method_definition" || sk == "class_body" {
        insert_sym(map, "this", CompletionKind::Variable, "this");
    }
}

fn collect_js_params(params: Node, source: &str, map: &mut BTreeMap<String, Symbol>) {
    let mut w = params.walk();
    for ch in params.named_children(&mut w) {
        collect_pattern_idents(ch, source, map, CompletionKind::Variable, "param");
    }
}

fn collect_js_import(node: Node, source: &str, map: &mut BTreeMap<String, Symbol>) {
    let mut w = node.walk();
    for ch in node.named_children(&mut w) {
        match ch.kind() {
            "import_clause" | "named_imports" | "namespace_import" | "identifier" => {
                if ch.kind() == "identifier" {
                    insert_sym(map, node_text(source, ch), CompletionKind::Module, "import");
                } else {
                    collect_js_import(ch, source, map);
                }
            }
            "import_specifier" => {
                // imported or alias
                if let Some(alias) = child_by_field(ch, "alias") {
                    insert_sym(map, node_text(source, alias), CompletionKind::Module, "import");
                } else if let Some(name) = child_by_field(ch, "name") {
                    insert_sym(map, node_text(source, name), CompletionKind::Module, "import");
                } else {
                    collect_pattern_idents(ch, source, map, CompletionKind::Module, "import");
                }
            }
            _ if ch.is_named() => collect_js_import(ch, source, map),
            _ => {}
        }
    }
}

fn builtins(lang: Lang) -> Vec<(&'static str, CompletionKind, &'static str)> {
    match lang {
        Lang::Rust => vec![
            ("Some", CompletionKind::Type, "prelude"),
            ("None", CompletionKind::Variable, "prelude"),
            ("Ok", CompletionKind::Type, "prelude"),
            ("Err", CompletionKind::Type, "prelude"),
            ("Vec", CompletionKind::Type, "prelude"),
            ("String", CompletionKind::Type, "prelude"),
            ("Option", CompletionKind::Type, "prelude"),
            ("Result", CompletionKind::Type, "prelude"),
            ("drop", CompletionKind::Function, "prelude"),
        ],
        Lang::Python => vec![
            ("True", CompletionKind::Variable, "builtin"),
            ("False", CompletionKind::Variable, "builtin"),
            ("None", CompletionKind::Variable, "builtin"),
            ("print", CompletionKind::Function, "builtin"),
            ("len", CompletionKind::Function, "builtin"),
            ("range", CompletionKind::Function, "builtin"),
            ("str", CompletionKind::Type, "builtin"),
            ("int", CompletionKind::Type, "builtin"),
            ("list", CompletionKind::Type, "builtin"),
            ("dict", CompletionKind::Type, "builtin"),
        ],
        Lang::JavaScript => vec![
            ("undefined", CompletionKind::Variable, "builtin"),
            ("null", CompletionKind::Variable, "builtin"),
            ("NaN", CompletionKind::Variable, "builtin"),
            ("Infinity", CompletionKind::Variable, "builtin"),
            ("console", CompletionKind::Variable, "builtin"),
            ("Object", CompletionKind::Type, "builtin"),
            ("Array", CompletionKind::Type, "builtin"),
            ("Promise", CompletionKind::Type, "builtin"),
            ("Math", CompletionKind::Variable, "builtin"),
            ("JSON", CompletionKind::Variable, "builtin"),
        ],
    }
}

fn matches_prefix(candidate: &str, prefix_lower: &str) -> bool {
    let c = candidate.to_ascii_lowercase();
    c.starts_with(prefix_lower)
}

fn add_snippet(
    items: &mut Vec<CompletionItem>,
    seen: &mut std::collections::BTreeSet<String>,
    prefix_lower: &str,
    trigger: &str,
    body: &str,
    detail: &str,
) {
    if matches_prefix(trigger, prefix_lower) && seen.insert(trigger.to_string()) {
        let insert = body.replace("${1}", "");
        items.push(CompletionItem {
            label: trigger.to_string(),
            kind: CompletionKind::Snippet,
            insert,
            detail: detail.into(),
        });
    }
}

/// Build completions from **lexical scope** + language keywords (not whole-file idents).
pub fn build_completions(
    prefix: &str,
    lang: Option<Lang>,
    lines: &[String],
    hl: &Highlighter,
    cursor_row: usize,
    cursor_col: usize,
) -> Vec<CompletionItem> {
    if prefix.is_empty() {
        return Vec::new();
    }
    let prefix_lower = prefix.to_ascii_lowercase();
    let mut items: Vec<CompletionItem> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();

    // Keywords for active language only
    for &kw in keywords_for(lang) {
        if matches_prefix(kw, &prefix_lower) && seen.insert(kw.to_string()) {
            items.push(CompletionItem {
                label: kw.to_string(),
                kind: CompletionKind::Keyword,
                insert: kw.to_string(),
                detail: "keyword".into(),
            });
        }
    }

    let source = hl.source();
    let cursor_byte = cursor_byte_offset(lines, cursor_row, cursor_col)
        .min(source.len().saturating_sub(0));

    if let (Some(lang), Some(tree)) = (lang, hl.tree_ref()) {
        let root = tree.root_node();
        for sym in scoped_symbols(lang, root, source, cursor_byte) {
            if sym.name == prefix {
                continue;
            }
            if matches_prefix(&sym.name, &prefix_lower) && seen.insert(sym.name.clone()) {
                items.push(CompletionItem {
                    label: sym.name.clone(),
                    kind: sym.kind,
                    insert: sym.name,
                    detail: format!("in scope · {}", sym.detail),
                });
            }
        }
    }

    // Snippets (language-specific triggers)
    if lang == Some(Lang::Rust) {
        add_snippet(&mut items, &mut seen, &prefix_lower, "pl", "println!(\"${1}\");", "snippet");
        add_snippet(&mut items, &mut seen, &prefix_lower, "fmt", "format!(\"${1}\")", "snippet");
    }
    if lang == Some(Lang::Python) {
        add_snippet(&mut items, &mut seen, &prefix_lower, "pr", "print(${1})", "snippet");
    }

    items.sort_by(|a, b| {
        let ap = a.label.to_ascii_lowercase().starts_with(&prefix_lower);
        let bp = b.label.to_ascii_lowercase().starts_with(&prefix_lower);
        // prefer non-keywords slightly for equal prefix? prefer shorter + in-scope detail
        let a_scope = a.detail.starts_with("in scope") as i32;
        let b_scope = b.detail.starts_with("in scope") as i32;
        bp.cmp(&ap)
            .then_with(|| b_scope.cmp(&a_scope))
            .then_with(|| a.label.len().cmp(&b.label.len()))
            .then_with(|| a.label.cmp(&b.label))
    });
    items.truncate(50);
    items
}

pub fn suggest(
    lines: &[String],
    cursor_row: usize,
    cursor_col: usize,
    hl: &Highlighter,
) -> Option<CompletionState> {
    let line = lines.get(cursor_row)?.as_str();
    let (start_col, prefix) = prefix_at(line, cursor_col);
    if prefix.is_empty() {
        return None;
    }
    if prefix.len() == 1 && !prefix.chars().next()?.is_ascii_alphabetic() {
        return None;
    }
    let lang = hl.lang;
    let items = build_completions(&prefix, lang, lines, hl, cursor_row, cursor_col);
    if items.is_empty() {
        return None;
    }
    Some(CompletionState {
        prefix,
        start_col,
        items,
        selected: 0,
    })
}
