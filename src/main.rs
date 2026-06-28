//! tui-code — VS Code-like TUI explorer + editor with tree-sitter highlighting.

mod app;
mod complete;
mod editor;
mod highlight;
mod shell_panel;
mod term;
mod theme;
mod tree;

use std::env;
use std::path::PathBuf;
use std::process;

use app::App;
use term::{force_restore_terminal, Terminal};

fn main() {
    // Always restore terminal on panic so the shell is usable.
    std::panic::set_hook(Box::new(|info| {
        force_restore_terminal();
        eprintln!("tui-code panicked: {info}");
    }));

    let args: Vec<String> = env::args().collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print_help();
        return;
    }

    let root = if args.len() > 1 && !args[1].starts_with('-') {
        PathBuf::from(&args[1])
    } else {
        env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    };

    let root = match root.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("tui-code: cannot open {}: {e}", root.display());
            process::exit(1);
        }
    };

    if !root.is_dir() {
        if root.is_file() {
            let parent = root.parent().unwrap_or(root.as_path()).to_path_buf();
            let parent = parent.canonicalize().unwrap_or(parent);
            run(parent, Some(root));
            return;
        }
        eprintln!("tui-code: not a directory: {}", root.display());
        process::exit(1);
    }

    run(root, None);
}

fn run(root: PathBuf, open_file: Option<PathBuf>) {
    let mut term = match Terminal::enter() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("tui-code: failed to enter raw mode: {e}");
            eprintln!("(needs an interactive terminal / TTY)");
            process::exit(1);
        }
    };

    let mut app = App::new(root);
    // Always start on the FILE TREE so ↑↓ navigation works immediately.
    // Tab / Enter-on-file moves focus into the editor; Esc returns to the tree.
    app.focus = app::Focus::Sidebar;
    if let Some(path) = open_file {
        if let Ok(buf) = editor::Buffer::from_path(&path) {
            let lang = buf.lang_label();
            let name = path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            app.buffers[0] = buf;
            // Select the opened file in the tree when possible.
            if let Some(i) = app.tree.entries.iter().position(|e| e.path == path) {
                app.tree.selected = i;
            } else {
                // try match by file name under root listing
                if let Some(i) = app
                    .tree
                    .entries
                    .iter()
                    .position(|e| e.name == name)
                {
                    app.tree.selected = i;
                }
            }
            app.status_msg = format!(
                "Loaded {name} ({lang}) — ↑↓ tree · Enter/Tab editor · Ctrl+C quit"
            );
        }
    } else {
        app.status_msg =
            "FILE TREE — ↑↓ move · Enter open · Tab editor · q or Ctrl+C quit".into();
    }

    if let Err(e) = app.draw(&mut term) {
        let _ = term.restore();
        eprintln!("draw error: {e}");
        process::exit(1);
    }

    while !app.quit {
        // Drain PTY output so integrated terminals stay live.
        app.poll_shells();
        if let Err(e) = app.draw(&mut term) {
            let _ = term.restore();
            eprintln!("draw error: {e}");
            process::exit(1);
        }
        match term.poll_input(50) {
            Ok(Some(input)) => {
                app.handle_input(input);
            }
            Ok(None) => {}
            Err(e) => {
                if e.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                let _ = term.restore();
                eprintln!("input error: {e}");
                process::exit(1);
            }
        }
    }

    let _ = term.restore();
}

fn print_help() {
    println!(
        "\
tui-code — terminal file explorer + editor (tree-sitter highlighting)

USAGE:
    tui-code [PATH]

MOUSE:
    Click files in the left tree to open them
    Click in the editor (or line numbers) to move the cursor
    Scroll wheel scrolls the pane under the pointer

QUIT (raw mode blocks OS Ctrl+C — app handles it):
    Ctrl+C              Quit immediately (always)
    Ctrl+Q / Ctrl+D     Quit (asks again if unsaved)
    q                   Quit when FILE TREE is focused
    Esc then q          From editor: Esc → tree, then q

FILE TREE:
    ↑ ↓ / j k           Move
    Enter               Open file or expand/collapse folder
    ← → / h l / Space   Collapse / expand folders
    Tab / Ctrl+B        Switch to editor
    Esc                 Focus file tree (from editor)

EDITOR:
    Arrows              Move cursor
    Ctrl+S              Save
    Esc                 Back to file tree

EXAMPLES:
    ./target/release/tui-code .
    ./target/release/tui-code examples/demo.rs
"
    );
}
