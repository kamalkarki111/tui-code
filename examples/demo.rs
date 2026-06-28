//! Demo file for tree-sitter syntax colors.
use std::collections::HashMap;

/// Returns a greeting.
pub fn greet(name: &str) -> String {
    let mut map: HashMap<&str, i32> = HashMap::new();
    map.insert("answer", 42);
    // line comment
    format!("Hello, {name}! answer={}", map["answer"])
}

#[derive(Debug)]
struct Point {
    x: f64,
    y: f64,
}

impl Point {
    fn origin() -> Self {
        Self { x: 0.0, y: 0.0 }
    }
}
