//! Symbol extraction from source files.
//!
//! Tier 1 (tree-sitter): Rust, Python, JavaScript, TypeScript, Go, Java, C, C++.
//! Tier 3 (heuristic regex): all other languages, or when tree-sitter fails.

use tree_sitter::{Language, Node, Parser};

/// The structural kind of an extracted symbol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymbolKind {
    Function,
    Method,
    Class,
    Struct,
    Enum,
    Trait,
    Interface,
    Const,
    Type,
}

impl SymbolKind {
    /// Returns the lowercase string representation used in queries and storage.
    pub fn as_str(&self) -> &'static str {
        match self {
            SymbolKind::Function => "function",
            SymbolKind::Method => "method",
            SymbolKind::Class => "class",
            SymbolKind::Struct => "struct",
            SymbolKind::Enum => "enum",
            SymbolKind::Trait => "trait",
            SymbolKind::Interface => "interface",
            SymbolKind::Const => "const",
            SymbolKind::Type => "type",
        }
    }
}

impl std::str::FromStr for SymbolKind {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "function" => Ok(SymbolKind::Function),
            "method" => Ok(SymbolKind::Method),
            "class" => Ok(SymbolKind::Class),
            "struct" => Ok(SymbolKind::Struct),
            "enum" => Ok(SymbolKind::Enum),
            "trait" => Ok(SymbolKind::Trait),
            "interface" => Ok(SymbolKind::Interface),
            "const" => Ok(SymbolKind::Const),
            "type" => Ok(SymbolKind::Type),
            _ => Err(()),
        }
    }
}

/// A symbol extracted from a source file.
#[derive(Debug, Clone)]
pub struct ExtractedSymbol {
    pub name: String,
    pub kind: SymbolKind,
    /// 1-based line number.
    pub line: u32,
    /// 0-based column.
    pub column: u32,
    /// True if extracted via heuristic fallback rather than tree-sitter.
    pub approximate: bool,
}

/// Detect tree-sitter Language from file extension. Returns None for unsupported types.
pub fn ts_language_for_path(path: &str) -> Option<Language> {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    match ext {
        "rs" => Some(tree_sitter_rust::LANGUAGE.into()),
        "py" => Some(tree_sitter_python::LANGUAGE.into()),
        "js" | "mjs" | "cjs" => Some(tree_sitter_javascript::LANGUAGE.into()),
        "ts" => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
        "tsx" => Some(tree_sitter_typescript::LANGUAGE_TSX.into()),
        "go" => Some(tree_sitter_go::LANGUAGE.into()),
        "java" => Some(tree_sitter_java::LANGUAGE.into()),
        "c" | "h" => Some(tree_sitter_c::LANGUAGE.into()),
        "cpp" | "cc" | "cxx" | "hpp" | "hxx" => Some(tree_sitter_cpp::LANGUAGE.into()),
        _ => None,
    }
}

/// Extract symbols from a source file using tree-sitter (preferred) or heuristics.
///
/// Never panics; returns an empty vec on total failure.
pub fn extract_symbols(path: &str, content: &[u8]) -> Vec<ExtractedSymbol> {
    let lang = ts_language_for_path(path);
    if let Some(language) = lang {
        let result = std::panic::catch_unwind(|| ts_extract(content, language));
        match result {
            Ok(symbols) if !symbols.is_empty() => return symbols,
            _ => {} // fall through to heuristic
        }
    }
    heuristic_extract(content)
}

/// Parse with tree-sitter and walk the AST for definition nodes.
fn ts_extract(content: &[u8], language: Language) -> Vec<ExtractedSymbol> {
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return heuristic_extract(content);
    }
    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return heuristic_extract(content),
    };
    let mut symbols = Vec::new();
    walk_node(tree.root_node(), content, false, &mut symbols);
    symbols
}

/// Recursively walk tree-sitter nodes, collecting definition sites.
fn walk_node(node: Node, src: &[u8], in_impl: bool, out: &mut Vec<ExtractedSymbol>) {
    let kind = node.kind();
    let inside_impl = in_impl
        || kind == "impl_item"
        || kind == "class_body"
        || kind == "class_declaration"
        || kind == "object_type";

    if let Some((sym_kind, name_field)) = definition_rule(node, in_impl) {
        if let Some(name_node) = node.child_by_field_name(name_field) {
            let name = name_node.utf8_text(src).unwrap_or("").to_string();
            if !name.is_empty()
                && name
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_alphanumeric() || c == '_')
            {
                let start = node.start_position();
                out.push(ExtractedSymbol {
                    name,
                    kind: sym_kind,
                    line: start.row as u32 + 1,
                    column: start.column as u32,
                    approximate: false,
                });
            }
        }
    }

    let count = node.child_count();
    for i in 0..count {
        if let Some(child) = node.child(i as u32) {
            walk_node(child, src, inside_impl, out);
        }
    }
}

/// Returns (SymbolKind, field_name_for_identifier) for known definition node kinds.
fn definition_rule(node: Node, in_impl: bool) -> Option<(SymbolKind, &'static str)> {
    match node.kind() {
        // Rust
        "function_item" => Some((
            if in_impl {
                SymbolKind::Method
            } else {
                SymbolKind::Function
            },
            "name",
        )),
        "struct_item" => Some((SymbolKind::Struct, "name")),
        "enum_item" => Some((SymbolKind::Enum, "name")),
        "trait_item" => Some((SymbolKind::Trait, "name")),
        "type_item" => Some((SymbolKind::Type, "name")),
        "const_item" => Some((SymbolKind::Const, "name")),
        // Python
        "async_function_definition" => Some((
            if in_impl {
                SymbolKind::Method
            } else {
                SymbolKind::Function
            },
            "name",
        )),
        "class_definition" => Some((SymbolKind::Class, "name")),
        // JavaScript / TypeScript
        "function_declaration" => Some((SymbolKind::Function, "name")),
        "method_definition" => Some((SymbolKind::Method, "name")),
        "class_declaration" => Some((SymbolKind::Class, "name")),
        "interface_declaration" => Some((SymbolKind::Interface, "name")),
        "type_alias_declaration" => Some((SymbolKind::Type, "name")),
        "enum_declaration" => Some((SymbolKind::Enum, "name")),
        // Go
        "method_declaration" => Some((SymbolKind::Method, "name")),
        "type_spec" => Some((SymbolKind::Type, "name")),
        // C / C++
        "function_definition" => {
            if node.child_by_field_name("name").is_some() {
                Some((
                    if in_impl {
                        SymbolKind::Method
                    } else {
                        SymbolKind::Function
                    },
                    "name",
                ))
            } else if node.child_by_field_name("declarator").is_some() {
                Some((SymbolKind::Function, "declarator"))
            } else {
                None
            }
        }
        "struct_specifier" | "class_specifier" => Some((SymbolKind::Struct, "name")),
        _ => None,
    }
}

/// Tier 3: regex-based heuristic for unsupported languages.
///
/// Matches common definition patterns. Results are marked `approximate: true`.
pub fn heuristic_extract(content: &[u8]) -> Vec<ExtractedSymbol> {
    let text = match std::str::from_utf8(content) {
        Ok(s) => s,
        Err(_) => return vec![],
    };

    // Pattern: optional visibility/async, definition keyword, identifier
    let re = match regex::Regex::new(
        r"(?m)^\s*(?:pub\s+)?(?:async\s+)?(?:def|fn|func|function|class|struct|enum|trait|interface|type)\s+([A-Za-z_]\w*)",
    ) {
        Ok(r) => r,
        Err(_) => return vec![],
    };

    let mut symbols = Vec::new();
    for cap in re.captures_iter(text) {
        let name = cap[1].to_string();
        // Find line number by counting newlines before match start.
        let byte_pos = cap.get(0).map(|m| m.start()).unwrap_or(0);
        let line = text[..byte_pos].chars().filter(|&c| c == '\n').count() as u32 + 1;
        symbols.push(ExtractedSymbol {
            name,
            kind: SymbolKind::Function,
            line,
            column: 0,
            approximate: true,
        });
    }
    symbols
}
