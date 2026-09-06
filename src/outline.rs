//! A map of a source file, so a large one can be navigated instead of pasted.
//!
//! `read` caps a file at 32 KiB. This module's own `mcp.rs` is well past
//! 280 KiB, which means the tool that edits it cannot see it. An outline turns
//! that file into a few hundred lines of `line  kind name`, `find_symbol`
//! locates a definition across the workspace, and `read_symbol` returns just
//! the one definition's source.
//!
//! Parsing is done with tree-sitter rather than regular expressions on
//! purpose: a brace- or indentation-matching heuristic gets confused by braces
//! inside string literals, which is exactly what a file full of `json!` macros
//! is made of, and a wrong symbol range sends an edit to the wrong lines.

use std::fs;
use std::path::Path;

use tree_sitter::{Node, Parser};

/// Files past this size are skipped rather than parsed. A source file this
/// large is generated or vendored, and parsing it during a workspace-wide
/// search would cost more than the result is worth.
const MAX_SOURCE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_SIGNATURE_CHARS: usize = 160;
pub const DEFAULT_MAX_SYMBOLS: usize = 400;
pub const HARD_MAX_SYMBOLS: usize = 2000;
pub const DEFAULT_MAX_MATCHES: usize = 50;
pub const HARD_MAX_MATCHES: usize = 200;
/// A workspace-wide search stops after this many candidate files.
const MAX_SEARCHED_FILES: usize = 5000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    Rust,
    Python,
    JavaScript,
    TypeScript,
    Tsx,
    Go,
}

impl Language {
    pub fn as_str(self) -> &'static str {
        match self {
            Language::Rust => "rust",
            Language::Python => "python",
            Language::JavaScript => "javascript",
            Language::TypeScript => "typescript",
            Language::Tsx => "tsx",
            Language::Go => "go",
        }
    }

    pub fn from_path(path: &Path) -> Option<Self> {
        let extension = path.extension()?.to_str()?.to_ascii_lowercase();
        match extension.as_str() {
            "rs" => Some(Language::Rust),
            "py" | "pyi" => Some(Language::Python),
            "js" | "jsx" | "mjs" | "cjs" => Some(Language::JavaScript),
            "ts" | "mts" | "cts" => Some(Language::TypeScript),
            "tsx" => Some(Language::Tsx),
            "go" => Some(Language::Go),
            _ => None,
        }
    }

    fn ts_language(self) -> tree_sitter::Language {
        match self {
            Language::Rust => tree_sitter_rust::LANGUAGE.into(),
            Language::Python => tree_sitter_python::LANGUAGE.into(),
            Language::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Language::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Language::Go => tree_sitter_go::LANGUAGE.into(),
        }
    }

    /// Node kinds that introduce a name worth listing. Anything else is walked
    /// through, so a method inside a class or an `impl` block is still found.
    fn definition_kinds(self) -> &'static [&'static str] {
        match self {
            Language::Rust => &[
                "function_item",
                "function_signature_item",
                "struct_item",
                "enum_item",
                "union_item",
                "trait_item",
                "impl_item",
                "type_item",
                "const_item",
                "static_item",
                "mod_item",
                "macro_definition",
            ],
            Language::Python => &["function_definition", "class_definition"],
            Language::JavaScript => &[
                "function_declaration",
                "generator_function_declaration",
                "class_declaration",
                "method_definition",
            ],
            Language::TypeScript | Language::Tsx => &[
                "function_declaration",
                "generator_function_declaration",
                "class_declaration",
                "abstract_class_declaration",
                "method_definition",
                "interface_declaration",
                "type_alias_declaration",
                "enum_declaration",
            ],
            // Go's const and var declarations are left out: listing every
            // package-level variable buries the functions.
            Language::Go => &[
                "function_declaration",
                "method_declaration",
                "type_declaration",
            ],
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Symbol {
    pub kind: String,
    pub name: String,
    /// 1-based, to match every other line number a caller sees.
    pub line: usize,
    pub end_line: usize,
    pub depth: usize,
    pub signature: String,
    pub start_byte: usize,
    pub end_byte: usize,
}

#[derive(Clone, Debug)]
pub struct Match {
    pub path: String,
    pub symbol: Symbol,
}

fn truncate_chars(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_string();
    }
    let mut out: String = value.chars().take(limit).collect();
    out.push('…');
    out
}

/// Read a file that is worth parsing, or nothing.
pub fn read_source(path: &Path) -> Option<String> {
    let metadata = fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_SOURCE_BYTES {
        return None;
    }
    fs::read_to_string(path).ok()
}

pub fn outline(source: &str, language: Language, max_symbols: usize) -> Vec<Symbol> {
    let mut parser = Parser::new();
    if parser.set_language(&language.ts_language()).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };
    // Indexing lines once keeps signature lookup off a per-symbol scan of the
    // whole file, which is quadratic on exactly the files this exists for.
    let lines: Vec<&str> = source.lines().collect();
    let mut symbols = Vec::new();
    collect(
        tree.root_node(),
        source,
        &lines,
        language,
        0,
        &mut symbols,
        max_symbols,
    );
    symbols
}

fn collect(
    node: Node<'_>,
    source: &str,
    lines: &[&str],
    language: Language,
    depth: usize,
    out: &mut Vec<Symbol>,
    max_symbols: usize,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if out.len() >= max_symbols {
            return;
        }
        if language.definition_kinds().contains(&child.kind())
            && let Some(symbol) = symbol_from(child, source, lines, depth)
        {
            out.push(symbol);
            // A definition's own body can hold more definitions: methods in a
            // class, functions in an `impl`, a closure's helper.
            collect(child, source, lines, language, depth + 1, out, max_symbols);
            continue;
        }
        collect(child, source, lines, language, depth, out, max_symbols);
    }
}

fn node_text<'a>(node: Node<'_>, source: &'a str) -> Option<&'a str> {
    source.get(node.byte_range())
}

/// Look for the name a definition introduces. Most grammars expose it as a
/// `name` field; the ones that do not keep it a short way down.
fn definition_name(node: Node<'_>, source: &str) -> Option<String> {
    if node.kind() == "impl_item" {
        let type_name = node
            .child_by_field_name("type")
            .and_then(|child| node_text(child, source));
        let trait_name = node
            .child_by_field_name("trait")
            .and_then(|child| node_text(child, source));
        return match (trait_name, type_name) {
            (Some(trait_name), Some(type_name)) => Some(format!("{trait_name} for {type_name}")),
            (None, Some(type_name)) => Some(type_name.to_string()),
            _ => None,
        };
    }
    if let Some(name) = node
        .child_by_field_name("name")
        .and_then(|child| node_text(child, source))
    {
        return Some(name.to_string());
    }
    first_identifier(node, source, 0)
}

fn first_identifier(node: Node<'_>, source: &str, depth: usize) -> Option<String> {
    if depth > 3 {
        return None;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind().ends_with("identifier") {
            return node_text(child, source).map(str::to_string);
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if let Some(name) = first_identifier(child, source, depth + 1) {
            return Some(name);
        }
    }
    None
}

fn symbol_from(node: Node<'_>, source: &str, lines: &[&str], depth: usize) -> Option<Symbol> {
    let name = definition_name(node, source)?;
    let start_row = node.start_position().row;
    let signature = lines
        .get(start_row)
        .map(|line| truncate_chars(line.trim(), MAX_SIGNATURE_CHARS))
        .unwrap_or_default();
    Some(Symbol {
        kind: node.kind().to_string(),
        name,
        line: start_row + 1,
        end_line: node.end_position().row + 1,
        depth,
        signature,
        start_byte: node.start_byte(),
        end_byte: node.end_byte(),
    })
}

/// Render an outline the way a caller reads it: one line per symbol, nesting
/// shown by indentation, line number first so it can be jumped to.
pub fn render(symbols: &[Symbol]) -> String {
    symbols
        .iter()
        .map(|symbol| {
            format!(
                "{:>6}  {}{} {}",
                symbol.line,
                "  ".repeat(symbol.depth),
                symbol.kind,
                symbol.name
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn find_in_source(source: &str, language: Language, needle: &str, exact: bool) -> Vec<Symbol> {
    outline(source, language, HARD_MAX_SYMBOLS)
        .into_iter()
        .filter(|symbol| {
            if exact {
                symbol.name == needle
            } else {
                symbol
                    .name
                    .to_ascii_lowercase()
                    .contains(&needle.to_ascii_lowercase())
            }
        })
        .collect()
}

/// Walk `root` for definitions matching `needle`.
///
/// Files are filtered on a plain substring first: parsing every source file in
/// a workspace to find one name would make the tool too slow to reach for.
pub fn find_symbol(
    root: &Path,
    workspace_root: &Path,
    needle: &str,
    exact: bool,
    max_matches: usize,
) -> (Vec<Match>, bool) {
    let mut matches = Vec::new();
    let mut searched = 0usize;
    let mut truncated = false;

    for entry in ignore::WalkBuilder::new(root)
        .hidden(true)
        .build()
        .flatten()
    {
        if matches.len() >= max_matches || searched >= MAX_SEARCHED_FILES {
            truncated = true;
            break;
        }
        let path = entry.path();
        let Some(language) = Language::from_path(path) else {
            continue;
        };
        let Some(source) = read_source(path) else {
            continue;
        };
        searched += 1;
        if !source.contains(needle) {
            continue;
        }
        let relative = path
            .strip_prefix(workspace_root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        for symbol in find_in_source(&source, language, needle, exact) {
            if matches.len() >= max_matches {
                truncated = true;
                break;
            }
            matches.push(Match {
                path: relative.clone(),
                symbol,
            });
        }
    }

    (matches, truncated)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUST_SOURCE: &str = r#"
use std::fmt;

const LIMIT: usize = 3;

pub struct Ledger {
    entries: Vec<u32>,
}

impl Ledger {
    pub fn new() -> Self {
        // A brace inside a string literal is what defeats a regex outline.
        let _noise = "{ fn not_a_function() {";
        Self { entries: Vec::new() }
    }

    fn total(&self) -> u32 {
        self.entries.iter().sum()
    }
}

impl fmt::Debug for Ledger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Ledger")
    }
}

pub fn audit(ledger: &Ledger) -> bool {
    ledger.total() > LIMIT as u32
}
"#;

    fn names(symbols: &[Symbol]) -> Vec<&str> {
        symbols.iter().map(|symbol| symbol.name.as_str()).collect()
    }

    #[test]
    fn a_rust_outline_lists_definitions_in_source_order_with_nesting() {
        let symbols = outline(RUST_SOURCE, Language::Rust, DEFAULT_MAX_SYMBOLS);

        assert_eq!(
            names(&symbols),
            vec![
                "LIMIT",
                "Ledger",
                "Ledger",
                "new",
                "total",
                "fmt::Debug for Ledger",
                "fmt",
                "audit",
            ]
        );
        let new = symbols
            .iter()
            .find(|symbol| symbol.name == "new")
            .expect("missing new");
        assert_eq!(new.depth, 1, "a method sits inside its impl block");
        assert_eq!(new.kind, "function_item");
        assert!(new.signature.starts_with("pub fn new()"));
    }

    #[test]
    fn a_brace_inside_a_string_does_not_invent_a_symbol() {
        let symbols = outline(RUST_SOURCE, Language::Rust, DEFAULT_MAX_SYMBOLS);

        assert!(
            !names(&symbols).contains(&"not_a_function"),
            "a string literal was parsed as code: {:?}",
            names(&symbols)
        );
    }

    #[test]
    fn a_symbol_range_covers_the_whole_definition() {
        let symbols = outline(RUST_SOURCE, Language::Rust, DEFAULT_MAX_SYMBOLS);
        let audit = symbols
            .iter()
            .find(|symbol| symbol.name == "audit")
            .expect("missing audit");

        let text = &RUST_SOURCE[audit.start_byte..audit.end_byte];
        assert!(text.starts_with("pub fn audit"));
        assert!(text.trim_end().ends_with('}'));
        assert!(audit.end_line > audit.line);
    }

    #[test]
    fn python_classes_carry_their_methods() {
        let source = "\
class Ledger:
    def __init__(self):
        self.entries = []

    def total(self):
        return sum(self.entries)


def audit(ledger):
    return ledger.total() > 3
";
        let symbols = outline(source, Language::Python, DEFAULT_MAX_SYMBOLS);

        assert_eq!(
            names(&symbols),
            vec!["Ledger", "__init__", "total", "audit"]
        );
        assert_eq!(symbols[1].depth, 1);
        assert_eq!(symbols[3].depth, 0);
    }

    #[test]
    fn typescript_interfaces_and_type_aliases_are_listed() {
        let source = "\
export interface Ledger {
  entries: number[];
}

export type Total = number;

export class Auditor {
  check(ledger: Ledger): boolean {
    return ledger.entries.length > 0;
  }
}
";
        let symbols = outline(source, Language::TypeScript, DEFAULT_MAX_SYMBOLS);

        assert_eq!(names(&symbols), vec!["Ledger", "Total", "Auditor", "check"]);
    }

    #[test]
    fn go_functions_methods_and_types_are_listed() {
        let source = "\
package ledger

type Ledger struct {
\tEntries []int
}

func (l *Ledger) Total() int {
\treturn len(l.Entries)
}

func Audit(l *Ledger) bool {
\treturn l.Total() > 3
}
";
        let symbols = outline(source, Language::Go, DEFAULT_MAX_SYMBOLS);

        assert_eq!(names(&symbols), vec!["Ledger", "Total", "Audit"]);
    }

    #[test]
    fn the_symbol_budget_is_respected() {
        let symbols = outline(RUST_SOURCE, Language::Rust, 3);
        assert_eq!(symbols.len(), 3);
    }

    #[test]
    fn a_search_matches_on_substring_unless_it_is_asked_for_exact() {
        let loose = find_in_source(RUST_SOURCE, Language::Rust, "tot", false);
        assert_eq!(names(&loose), vec!["total"]);

        let exact = find_in_source(RUST_SOURCE, Language::Rust, "tot", true);
        assert!(exact.is_empty());

        let exact_hit = find_in_source(RUST_SOURCE, Language::Rust, "total", true);
        assert_eq!(names(&exact_hit), vec!["total"]);
    }

    #[test]
    fn languages_are_recognised_by_extension() {
        assert_eq!(
            Language::from_path(Path::new("a/b.rs")),
            Some(Language::Rust)
        );
        assert_eq!(
            Language::from_path(Path::new("a/b.tsx")),
            Some(Language::Tsx)
        );
        assert_eq!(
            Language::from_path(Path::new("a/b.TS")),
            Some(Language::TypeScript)
        );
        assert_eq!(Language::from_path(Path::new("a/b.md")), None);
    }

    #[test]
    fn a_workspace_search_reports_the_path_relative_to_the_workspace() {
        let root =
            std::env::temp_dir().join(format!("catdesk-outline-search-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("src")).expect("create tree");
        std::fs::write(root.join("src/ledger.rs"), RUST_SOURCE).expect("write source");
        std::fs::write(root.join("src/notes.md"), "fn audit() {}").expect("write note");

        let (matches, _) = find_symbol(&root, &root, "audit", true, DEFAULT_MAX_MATCHES);

        assert_eq!(matches.len(), 1, "only the parsed source should match");
        assert_eq!(matches[0].path, "src/ledger.rs");
        assert_eq!(matches[0].symbol.name, "audit");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_rendered_outline_leads_with_the_line_number() {
        let symbols = outline(RUST_SOURCE, Language::Rust, DEFAULT_MAX_SYMBOLS);
        let rendered = render(&symbols);
        let first = rendered.lines().next().expect("empty outline");

        assert!(first.trim_start().starts_with('4'), "got {first:?}");
        assert!(rendered.contains("  function_item new"));
    }
}
