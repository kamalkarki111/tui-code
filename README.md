# tui-code

VS Code–style **terminal** file explorer + editor with **tree-sitter AST syntax highlighting** in the TUI (Rust / Python / JavaScript).

No syntect. No HTML export. Colors are applied live in the editor pane from the AST.

## Build & run

```bash
cargo build --release

# open a Rust file — highlighting appears in the right-hand editor
./target/release/tui-code examples/demo.rs
./target/release/tui-code src/main.rs

# or a project folder, then Enter on a file in Explorer
./target/release/tui-code .
```

## Highlighting behavior

| Step | Behavior |
|------|----------|
| Open `.rs` / `.py` / `.js` | Full tree-sitter parse → AST |
| Type / delete | `InputEdit` on the tree, then **incremental** reparse (not whole file from scratch) |
| Paint frame | Only **visible** editor lines; leaf nodes intersecting each line are colored by `node.kind()` |

Status bar shows the language (`rust`, `python`, `javascript`, or `plaintext`).

## Keys

| Key | Action |
|-----|--------|
| `Ctrl+Q` | Quit |
| `Ctrl+S` | Save |
| `Ctrl+B` / `Tab` | Explorer ↔ Editor |
| `Enter` | Open file / toggle folder |
| Arrows | Move in editor |
| `?` | Help |

## Layout

```
┌ tui-code  ·  /project ──────────────────────┐
│ EXPLORER    │  demo.rs                       │
│ ▾ examples  │  1  //! comment…               │  ← AST colors
│   demo.rs   │  2  use std::…                 │
│             │  5  pub fn greet…              │
│ demo.rs  EDITOR  Ln 1, Col 1  rust  UTF-8    │
└──────────────────────────────────────────────┘
```
