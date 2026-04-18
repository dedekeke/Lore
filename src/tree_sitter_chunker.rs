use tree_sitter::{Node, Parser, Tree};

#[derive(Debug, Clone, serde::Serialize)]
pub struct AstChunk {
    pub name: Option<String>,
    pub kind: String,
    pub start_line: usize,
    pub end_line: usize,
    pub visibility: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CodeEdge {
    pub source_entity: String,
    pub target_entity: String,
    pub edge_type: String,
    pub source_file: Option<String>,
    pub target_file: Option<String>,
}

pub fn get_parser(language: &str) -> Option<Parser> {
    let mut parser = Parser::new();
    let lang: tree_sitter::Language = match language {
        "rust" => tree_sitter_rust::LANGUAGE.into(),
        "python" => tree_sitter_python::LANGUAGE.into(),
        "javascript" => tree_sitter_javascript::LANGUAGE.into(),
        "typescript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        "go" => tree_sitter_go::LANGUAGE.into(),
        "java" => tree_sitter_java::LANGUAGE.into(),
        "c" => tree_sitter_c::LANGUAGE.into(),
        "cpp" => tree_sitter_cpp::LANGUAGE.into(),
        _ => return None,
    };
    parser.set_language(&lang).ok()?;
    Some(parser)
}

/// Parse file with tree-sitter and extract top-level AST chunks.
/// Returns (chunks, tree) so the tree can be reused for edge extraction without re-parsing.
/// Returns None if no grammar exists or parse fails.
pub fn chunk_file_ast(content: &str, language: &str) -> Option<(Vec<AstChunk>, Tree)> {
    let mut parser = get_parser(language)?;
    let tree = parser.parse(content, None)?;
    let root = tree.root_node();
    let mut chunks = Vec::new();

    let target_kinds = target_node_kinds(language);

    collect_chunks(&root, content, language, &target_kinds, &mut chunks);

    if chunks.is_empty() {
        return None;
    }

    chunks.sort_by_key(|c| c.start_line);
    Some((chunks, tree))
}

/// Node kinds we extract as top-level chunks per language
fn target_node_kinds(language: &str) -> Vec<&'static str> {
    match language {
        "rust" => vec![
            "function_item",
            "struct_item",
            "enum_item",
            "impl_item",
            "trait_item",
            "mod_item",
            "const_item",
            "type_item",
        ],
        "python" => vec!["function_definition", "class_definition"],
        "javascript" => vec![
            "function_declaration",
            "class_declaration",
            "lexical_declaration",
            "export_statement",
        ],
        // TS/TSX share JS AST structure
        "typescript" | "tsx" => vec![
            "function_declaration",
            "class_declaration",
            "lexical_declaration",
            "export_statement",
        ],
        "go" => vec![
            "function_declaration",
            "method_declaration",
            "type_declaration",
        ],
        "java" => vec![
            "class_declaration",
            "method_declaration",
            "interface_declaration",
        ],
        "c" => vec!["function_definition", "struct_specifier"],
        "cpp" => vec!["function_definition", "struct_specifier", "class_specifier"],
        _ => vec![],
    }
}

fn collect_chunks(
    node: &Node,
    content: &str,
    language: &str,
    target_kinds: &[&str],
    out: &mut Vec<AstChunk>,
) {
    let kind = node.kind();
    if target_kinds.contains(&kind) {
        let name = extract_node_name(node, content, language);
        let visibility = extract_visibility(node, content, language);
        out.push(AstChunk {
            name,
            kind: normalize_kind(kind),
            start_line: node.start_position().row + 1,
            end_line: node.end_position().row + 1,
            visibility,
        });
        return; // don't recurse into matched nodes
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_chunks(&child, content, language, target_kinds, out);
    }
}

fn extract_node_name(node: &Node, content: &str, language: &str) -> Option<String> {
    // For export_statement, dig into the child declaration
    if node.kind() == "export_statement" {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            let ck = child.kind();
            if ck == "function_declaration"
                || ck == "class_declaration"
                || ck == "lexical_declaration"
            {
                return extract_node_name(&child, content, language);
            }
        }
        return None;
    }

    // For lexical_declaration (const x = ...), find the variable_declarator name
    if node.kind() == "lexical_declaration" {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "variable_declarator" {
                if let Some(name_node) = child.child_by_field_name("name") {
                    return Some(node_text(name_node, content).to_string());
                }
            }
        }
        return None;
    }

    // Most languages use a "name" field on the node
    if let Some(name_node) = node.child_by_field_name("name") {
        return Some(node_text(name_node, content).to_string());
    }

    // Rust impl: `impl Trait for Type` or `impl Type` — extract type name
    if language == "rust" && node.kind() == "impl_item" {
        if let Some(type_node) = node.child_by_field_name("type") {
            let type_name = node_text(type_node, content);
            if let Some(trait_node) = node.child_by_field_name("trait") {
                let trait_name = node_text(trait_node, content);
                return Some(format!("{trait_name} for {type_name}"));
            }
            return Some(type_name.to_string());
        }
    }

    None
}

fn extract_visibility(node: &Node, content: &str, language: &str) -> Option<String> {
    match language {
        "rust" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "visibility_modifier" {
                    return Some(node_text(child, content).to_string());
                }
            }
            None
        }
        "java" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "modifiers" {
                    let text = node_text(child, content);
                    if text.contains("public") {
                        return Some("public".to_string());
                    }
                    if text.contains("private") {
                        return Some("private".to_string());
                    }
                    if text.contains("protected") {
                        return Some("protected".to_string());
                    }
                }
            }
            None
        }
        "javascript" | "typescript" => {
            // Check parent for export_statement
            if let Some(parent) = node.parent() {
                if parent.kind() == "export_statement" {
                    return Some("export".to_string());
                }
            }
            None
        }
        _ => None,
    }
}

fn normalize_kind(kind: &str) -> String {
    match kind {
        "function_item" | "function_definition" | "function_declaration" | "method_declaration" => {
            "function".to_string()
        }
        "struct_item" | "struct_specifier" => "struct".to_string(),
        "enum_item" => "enum".to_string(),
        "impl_item" => "impl".to_string(),
        "trait_item" => "trait".to_string(),
        "mod_item" => "module".to_string(),
        "const_item" | "lexical_declaration" => "const".to_string(),
        "type_item" | "type_declaration" => "type".to_string(),
        "class_definition" | "class_declaration" | "class_specifier" => "class".to_string(),
        "interface_declaration" => "interface".to_string(),
        "export_statement" => "export".to_string(),
        other => other.to_string(),
    }
}

/// Extract call edges from parsed tree. Walks all call_expression nodes.
pub fn extract_calls(tree: &Tree, content: &str, file_path: &str) -> Vec<CodeEdge> {
    let mut edges = Vec::new();
    walk_calls(&tree.root_node(), content, file_path, None, &mut edges);
    edges
}

fn walk_calls(
    node: &Node,
    content: &str,
    file_path: &str,
    enclosing_fn: Option<&str>,
    out: &mut Vec<CodeEdge>,
) {
    let kind = node.kind();

    // Track enclosing function scope
    let current_fn = match kind {
        "function_item"
        | "function_definition"
        | "function_declaration"
        | "method_declaration"
        | "method_definition" => node
            .child_by_field_name("name")
            .map(|n| node_text(n, content).to_string()),
        _ => None,
    };
    let fn_name = current_fn.as_deref().or(enclosing_fn);

    // Extract call targets (including Rust macro invocations like println!(), vec!())
    if kind == "call_expression" || kind == "macro_invocation" {
        let callee = if kind == "macro_invocation" {
            node.child_by_field_name("macro")
                .map(|n| node_text(n, content).to_string())
        } else {
            extract_callee(node, content)
        };
        if let Some(callee) = callee {
            let source = fn_name.unwrap_or("<module>");
            out.push(CodeEdge {
                source_entity: source.to_string(),
                target_entity: callee,
                edge_type: "calls".to_string(),
                source_file: Some(file_path.to_string()),
                target_file: None,
            });
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_calls(&child, content, file_path, fn_name, out);
    }
}

fn extract_callee(node: &Node, content: &str) -> Option<String> {
    // call_expression has a "function" field in most grammars
    let func_node = node.child_by_field_name("function")?;
    let text = node_text(func_node, content);
    // Strip long paths, keep last two segments max: `foo::bar::baz` -> `bar::baz`
    let parts: Vec<&str> = text.split("::").collect();
    let callee = if parts.len() > 2 {
        parts[parts.len() - 2..].join("::")
    } else {
        text.to_string()
    };
    if callee.is_empty() {
        None
    } else {
        Some(callee)
    }
}

/// Extract import/use edges from parsed tree.
pub fn extract_imports(tree: &Tree, content: &str, file_path: &str) -> Vec<CodeEdge> {
    let mut edges = Vec::new();
    walk_imports(&tree.root_node(), content, file_path, &mut edges);
    edges
}

fn walk_imports(node: &Node, content: &str, file_path: &str, out: &mut Vec<CodeEdge>) {
    let kind = node.kind();

    let is_import = matches!(
        kind,
        "use_declaration" | "import_statement" | "import_declaration" | "import_from_statement"
    );

    if is_import {
        let text = node_text(*node, content).to_string();
        let module = clean_import_path(&text);
        if !module.is_empty() {
            out.push(CodeEdge {
                source_entity: file_path.to_string(),
                target_entity: module,
                edge_type: "uses".to_string(),
                source_file: Some(file_path.to_string()),
                target_file: None,
            });
        }
        return; // no need to recurse into import nodes
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_imports(&child, content, file_path, out);
    }
}

/// Clean import path: strip keywords, semicolons, braces for a compact module reference.
/// `use std::collections::HashMap;` -> `std::collections::HashMap`
/// `from foo import bar` -> `foo`
/// `import { x } from 'y'` -> `y`
fn clean_import_path(text: &str) -> String {
    let trimmed = text.trim();

    // Rust: `use path;`
    if let Some(rest) = trimmed.strip_prefix("use ") {
        return rest
            .trim_end_matches(';')
            .trim()
            .split('{')
            .next()
            .unwrap_or("")
            .trim_end_matches("::")
            .trim()
            .to_string();
    }

    // JS/TS: `import ... from 'path'` — check before Python `from` to handle
    // `import { foo } from 'bar'` correctly (rfind avoids matching `from` in braces)
    if trimmed.starts_with("import ") && trimmed.contains(" from ") {
        if let Some(idx) = trimmed.rfind(" from ") {
            let after = &trimmed[idx + 6..];
            return after
                .trim()
                .trim_matches(|c: char| c == '\'' || c == '"' || c == ';' || c == ' ')
                .to_string();
        }
    }

    // Python: `from mod import ...` or `import mod`
    if let Some(rest) = trimmed.strip_prefix("from ") {
        return rest.split_whitespace().next().unwrap_or("").to_string();
    }
    if let Some(rest) = trimmed.strip_prefix("import ") {
        return rest
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_end_matches(';')
            .to_string();
    }

    trimmed.to_string()
}

// tree-sitter byte offsets align to UTF-8 boundaries, so byte slicing is safe here
fn node_text<'a>(node: Node<'a>, content: &'a str) -> &'a str {
    let start = node.start_byte();
    let end = node.end_byte();
    &content[start..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chunk_rust_ast() {
        let src = r#"
use std::io;

pub fn hello() {
    println!("hi");
}

struct Foo {
    x: i32,
}

impl Foo {
    fn bar(&self) -> i32 {
        self.x
    }
}

enum Color {
    Red,
    Blue,
}
"#;
        let (chunks, _tree) = chunk_file_ast(src, "rust").unwrap();
        assert!(chunks.len() >= 4, "got {} chunks", chunks.len());

        let fn_chunk = chunks.iter().find(|c| c.kind == "function").unwrap();
        assert_eq!(fn_chunk.name.as_deref(), Some("hello"));
        assert_eq!(fn_chunk.visibility.as_deref(), Some("pub"));

        let struct_chunk = chunks.iter().find(|c| c.kind == "struct").unwrap();
        assert_eq!(struct_chunk.name.as_deref(), Some("Foo"));

        let impl_chunk = chunks.iter().find(|c| c.kind == "impl").unwrap();
        assert_eq!(impl_chunk.name.as_deref(), Some("Foo"));

        let enum_chunk = chunks.iter().find(|c| c.kind == "enum").unwrap();
        assert_eq!(enum_chunk.name.as_deref(), Some("Color"));
    }

    #[test]
    fn test_chunk_python_ast() {
        let src = r#"
import os

def greet(name):
    print(f"Hello {name}")

class Person:
    def __init__(self, name):
        self.name = name
"#;
        let (chunks, _tree) = chunk_file_ast(src, "python").unwrap();
        assert!(chunks.len() >= 2);
        assert!(chunks.iter().any(|c| c.kind == "function"));
        assert!(chunks.iter().any(|c| c.kind == "class"));
    }

    #[test]
    fn test_chunk_javascript_ast() {
        let src = r#"
import { foo } from 'bar';

function doStuff() {
    return 42;
}

class Widget {
    render() {}
}

const LIMIT = 100;
"#;
        let (chunks, _tree) = chunk_file_ast(src, "javascript").unwrap();
        assert!(chunks.len() >= 3, "got {} chunks", chunks.len());
    }

    #[test]
    fn test_returns_none_for_unknown_language() {
        assert!(chunk_file_ast("hello", "brainfuck").is_none());
    }

    #[test]
    fn test_extract_rust_calls() {
        let src = r#"
fn caller() {
    helper();
    foo::bar();
}

fn helper() {}
"#;
        let (_chunks, tree) = chunk_file_ast(src, "rust").unwrap();
        let edges = extract_calls(&tree, src, "src/lib.rs");
        assert!(!edges.is_empty());
        assert!(edges.iter().any(|e| e.target_entity == "helper"));
    }

    #[test]
    fn test_extract_rust_macro_calls() {
        let src = r#"
fn main() {
    println!("hello");
    vec![1, 2, 3];
}
"#;
        let (_chunks, tree) = chunk_file_ast(src, "rust").unwrap();
        let edges = extract_calls(&tree, src, "src/main.rs");
        assert!(edges.iter().any(|e| e.target_entity.contains("println")));
    }

    #[test]
    fn test_extract_rust_imports() {
        let src = r#"
use std::collections::HashMap;
use crate::db;

fn main() {}
"#;
        let (_chunks, tree) = chunk_file_ast(src, "rust").unwrap();
        let edges = extract_imports(&tree, src, "src/main.rs");
        assert_eq!(edges.len(), 2);
        assert!(edges
            .iter()
            .any(|e| e.target_entity == "std::collections::HashMap"));
        assert!(edges.iter().any(|e| e.target_entity == "crate::db"));
    }

    #[test]
    fn test_extract_python_imports() {
        let src = "from os.path import join\nimport sys\n\ndef main():\n    pass\n";
        let (_chunks, tree) = chunk_file_ast(src, "python").unwrap();
        let edges = extract_imports(&tree, src, "app.py");
        assert!(edges.len() >= 2);
    }

    #[test]
    fn test_clean_import_path() {
        assert_eq!(clean_import_path("use std::io;"), "std::io");
        assert_eq!(
            clean_import_path("use std::collections::{HashMap, HashSet};"),
            "std::collections"
        );
        assert_eq!(clean_import_path("from os import path"), "os");
        assert_eq!(clean_import_path("import sys"), "sys");
        assert_eq!(clean_import_path("import { foo } from 'bar';"), "bar");
    }
}
