//! Repository map: extracts top-level definitions (functions, structs,
//! classes, etc.) from source files using tree-sitter, producing a compact
//! outline suitable for LLM context.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ignore::WalkBuilder;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor};

#[derive(Debug, thiserror::Error)]
pub enum RepoMapError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("tree-sitter error: {0}")]
    Language(String),
}

/// A single extracted definition.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Definition {
    /// e.g. "function", "struct", "class", "impl", "trait", "method"
    pub kind: String,
    /// Signature line(s), trimmed, without body.
    pub signature: String,
    /// 1-based start line.
    pub line: usize,
}

/// Map of relative file path -> its definitions.
pub type RepoMap = BTreeMap<PathBuf, Vec<Definition>>;

struct LangSpec {
    extensions: &'static [&'static str],
    language: tree_sitter::Language,
    query_src: &'static str,
    kinds: &'static [&'static str],
}

fn lang_specs() -> Vec<LangSpec> {
    vec![
        LangSpec {
            extensions: &["rs"],
            language: tree_sitter_rust::LANGUAGE.into(),
            query_src: r#"
                (function_item) @def
                (struct_item) @def
                (enum_item) @def
                (trait_item) @def
                (impl_item) @def
            "#,
            kinds: &["function", "struct", "enum", "trait", "impl"],
        },
        LangSpec {
            extensions: &["py"],
            language: tree_sitter_python::LANGUAGE.into(),
            query_src: r#"
                (function_definition) @def
                (class_definition) @def
            "#,
            kinds: &["function", "class"],
        },
        LangSpec {
            extensions: &["js", "jsx"],
            language: tree_sitter_javascript::LANGUAGE.into(),
            query_src: r#"
                (function_declaration) @def
                (class_declaration) @def
                (method_definition) @def
            "#,
            kinds: &["function", "class", "method"],
        },
        LangSpec {
            extensions: &["ts", "tsx"],
            language: tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            query_src: r#"
                (function_declaration) @def
                (class_declaration) @def
                (interface_declaration) @def
                (method_definition) @def
            "#,
            kinds: &["function", "class", "interface", "method"],
        },
        LangSpec {
            extensions: &["go"],
            language: tree_sitter_go::LANGUAGE.into(),
            query_src: r#"
                (function_declaration) @def
                (method_declaration) @def
                (type_declaration) @def
            "#,
            kinds: &["function", "method", "type"],
        },
    ]
}

/// Extract the signature: first line of the node, trimmed, with trailing
/// `{` removed. Multi-line signatures are cut at 200 chars.
fn signature_of(source: &str, node: tree_sitter::Node) -> String {
    let start = node.start_byte();
    let text = &source[start..node.end_byte()];
    let first_line = text.lines().next().unwrap_or("").trim();
    let sig = first_line.trim_end_matches('{').trim_end();
    let mut sig = sig.to_string();
    if sig.len() > 200 {
        sig.truncate(200);
        sig.push_str("...");
    }
    sig
}

/// Parse a single file and return its definitions, or empty vec for
/// unsupported extensions.
pub fn map_file(path: &Path, source: &str) -> Result<Vec<Definition>, RepoMapError> {
    let ext = match path.extension().and_then(|e| e.to_str()) {
        Some(e) => e,
        None => return Ok(vec![]),
    };
    let specs = lang_specs();
    let spec = match specs.iter().find(|s| s.extensions.contains(&ext)) {
        Some(s) => s,
        None => return Ok(vec![]),
    };

    let mut parser = Parser::new();
    parser
        .set_language(&spec.language)
        .map_err(|e| RepoMapError::Language(e.to_string()))?;
    let tree = match parser.parse(source, None) {
        Some(t) => t,
        None => return Ok(vec![]),
    };

    let query = Query::new(&spec.language, spec.query_src)
        .map_err(|e| RepoMapError::Language(e.to_string()))?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());

    let mut defs = Vec::new();
    while let Some(m) = matches.next() {
        let kind = spec
            .kinds
            .get(m.pattern_index)
            .copied()
            .unwrap_or("definition");
        for cap in m.captures {
            defs.push(Definition {
                kind: kind.to_string(),
                signature: signature_of(source, cap.node),
                line: cap.node.start_position().row + 1,
            });
        }
    }
    defs.sort_by_key(|d| d.line);
    Ok(defs)
}

/// Walk the workspace (respecting .gitignore) and build the full map.
/// Files larger than `max_file_bytes` are skipped.
pub fn build_repo_map(root: &Path, max_file_bytes: u64) -> Result<RepoMap, RepoMapError> {
    let mut map = RepoMap::new();
    let walker = WalkBuilder::new(root).hidden(true).git_ignore(true).build();

    for entry in walker.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if let Ok(meta) = path.metadata()
            && meta.len() > max_file_bytes
        {
            continue;
        }
        let Ok(source) = std::fs::read_to_string(path) else {
            continue;
        };
        let defs = map_file(path, &source)?;
        if !defs.is_empty() {
            let rel = path.strip_prefix(root).unwrap_or(path).to_path_buf();
            map.insert(rel, defs);
        }
    }
    Ok(map)
}

/// Render the map as compact text for LLM consumption, bounded by
/// `max_bytes`. Format:
/// ```text
/// src/agent.rs:
///   12: fn run_turn(&mut self, input: &str) -> Result<Turn>
///   80: struct AgentLoop
/// ```
pub fn render_map(map: &RepoMap, max_bytes: usize) -> String {
    let mut out = String::new();
    for (path, defs) in map {
        let mut block = format!("{}:\n", path.display());
        for d in defs {
            block.push_str(&format!("  {}: {}\n", d.line, d.signature));
        }
        if out.len() + block.len() > max_bytes {
            out.push_str("... (truncated)\n");
            break;
        }
        out.push_str(&block);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_definitions_extracted() {
        let src =
            "pub fn hello(name: &str) -> String {\n    name.into()\n}\n\nstruct Point { x: i32 }\n";
        let defs = map_file(Path::new("test.rs"), src).unwrap();
        assert_eq!(defs.len(), 2);
        assert_eq!(defs[0].kind, "function");
        assert!(defs[0].signature.contains("pub fn hello"));
        assert!(!defs[0].signature.contains('{'));
        assert_eq!(defs[1].kind, "struct");
    }

    #[test]
    fn python_definitions_extracted() {
        let src = "def foo(a, b):\n    pass\n\nclass Bar:\n    def method(self): ...\n";
        let defs = map_file(Path::new("test.py"), src).unwrap();
        assert!(defs.iter().any(|d| d.kind == "class"));
        assert!(defs.iter().any(|d| d.signature.contains("def foo")));
    }

    #[test]
    fn unsupported_extension_returns_empty() {
        let defs = map_file(Path::new("data.csv"), "a,b,c").unwrap();
        assert!(defs.is_empty());
    }

    #[test]
    fn render_respects_budget() {
        let mut map = RepoMap::new();
        map.insert(
            PathBuf::from("a.rs"),
            vec![Definition {
                kind: "function".into(),
                signature: "fn a()".into(),
                line: 1,
            }],
        );
        let rendered = render_map(&map, 10_000);
        assert!(rendered.contains("a.rs:"));
        assert!(rendered.contains("1: fn a()"));
    }
}
