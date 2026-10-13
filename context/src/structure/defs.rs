//! Definition-line detection for source code.
//!
//! Heuristic, line-based, and language-specific: good enough to name the
//! unit a passage belongs to, attach doc comments, and seed the entity graph
//! with definition sites, without a parser per language. Each language's
//! rules are compiled once into a `RegexSet` so a non-matching line costs a
//! single automaton pass.

use std::sync::OnceLock;

use regex::{Regex, RegexSet};

use super::defs_rules::*;
use super::lang::Lang;
use crate::types::SymbolKind;

/// A detected definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Def {
    /// Symbol name (last path segment for qualified names).
    pub name: String,
    /// What kind of definition the line introduces.
    pub kind: SymbolKind,
    /// For `impl Trait for Type`, the trait name.
    pub implements: Option<String>,
}

/// How a rule derives the symbol kind.
#[derive(Clone, Copy)]
pub(super) enum KindSpec {
    Fixed(SymbolKind),
    /// Like `Fixed`, for rules that infer a definition from `name(` syntax
    /// alone; names in [`NOT_NAMES`] (`if`, `while`, ...) are rejected.
    Guarded(SymbolKind),
    /// Map the keyword captured in group 1; the name is in group 2.
    Keyword,
}

struct Rule {
    re: Regex,
    kind: KindSpec,
}

/// Constructor of one language's rule list.
pub(super) type RuleSpecs = fn() -> Vec<(&'static str, KindSpec)>;

struct RuleSet {
    set: RegexSet,
    rules: Vec<Rule>,
}

/// Words that can precede `(` without being a definition name. Only checked
/// for [`KindSpec::Guarded`] rules: `fn new` is a fine Rust definition.
const NOT_NAMES: &[&str] = &[
    "assert", "await", "case", "catch", "delete", "do", "elif", "else", "fixed", "for", "foreach",
    "function", "if", "lock", "match", "new", "return", "sizeof", "switch", "throw", "try",
    "typeof", "unless", "until", "using", "when", "while", "with", "yield",
];

/// Detect a definition introduced on `line`.
pub(crate) fn detect(line: &str, lang: Lang) -> Option<Def> {
    if lang == Lang::Rust {
        if let Some(def) = rust_impl(line) {
            return Some(def);
        }
    }
    let rules = rules_for(lang)?;
    let first = rules.set.matches(line).into_iter().next()?;
    let rule = &rules.rules[first];
    let caps = rule.re.captures(line)?;
    let (kind, name) = match rule.kind {
        KindSpec::Fixed(kind) => (kind, caps.get(1)?.as_str()),
        KindSpec::Guarded(kind) => {
            let name = caps.get(1)?.as_str();
            if NOT_NAMES.contains(&name) {
                return None;
            }
            (kind, name)
        }
        KindSpec::Keyword => {
            let keyword = caps.get(1)?.as_str().to_ascii_lowercase();
            (keyword_kind(&keyword)?, caps.get(2)?.as_str())
        }
    };
    let name = last_segment(name.trim_matches('"'));
    if name.is_empty() {
        return None;
    }
    // A qualified C++ name (`Foo::bar`) is a method of `Foo`.
    let kind = if kind == SymbolKind::Function && caps[0].contains("::") && lang == Lang::Cpp {
        SymbolKind::Method
    } else {
        kind
    };
    Some(Def {
        name: name.to_string(),
        kind,
        implements: None,
    })
}

fn last_segment(name: &str) -> &str {
    name.rsplit(['.', ':'])
        .find(|s| !s.is_empty())
        .unwrap_or(name)
}

fn keyword_kind(keyword: &str) -> Option<SymbolKind> {
    Some(match keyword {
        "class" | "object" => SymbolKind::Class,
        "interface" | "trait" | "protocol" => SymbolKind::Interface,
        "enum" => SymbolKind::Enum,
        "struct" | "record" => SymbolKind::Struct,
        "extension" => SymbolKind::Impl,
        "table" | "view" | "function" | "procedure" | "index" | "trigger" | "type" => {
            SymbolKind::Schema
        }
        _ => return None,
    })
}

/// Parse a Rust `impl` header by hand: generic parameter lists nest `<>`,
/// which a regex cannot balance.
fn rust_impl(line: &str) -> Option<Def> {
    let t = line.trim_start();
    let t = t.strip_prefix("unsafe ").unwrap_or(t).trim_start();
    let rest = t.strip_prefix("impl")?;
    if !(rest.starts_with(' ') || rest.starts_with('<')) {
        return None;
    }
    let rest = skip_generics(rest.trim_start()).trim_start();
    let (first, after) = take_path(rest)?;
    let after = skip_generics(after).trim_start();
    if let Some(ty) = after.strip_prefix("for ") {
        let (ty, _) = take_path(ty.trim_start())?;
        return Some(Def {
            name: last_segment(ty).to_string(),
            kind: SymbolKind::Impl,
            implements: Some(last_segment(first).to_string()),
        });
    }
    Some(Def {
        name: last_segment(first).to_string(),
        kind: SymbolKind::Impl,
        implements: None,
    })
}

/// Skip one balanced `<...>` group at the start of `s`, if present.
fn skip_generics(s: &str) -> &str {
    if !s.starts_with('<') {
        return s;
    }
    let mut depth = 0usize;
    for (i, c) in s.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return &s[i + 1..];
                }
            }
            _ => {}
        }
    }
    ""
}

/// Take a path like `crate::index::Index` (with optional leading `&`/`dyn`).
fn take_path(s: &str) -> Option<(&str, &str)> {
    let s = s.trim_start_matches('&').trim_start();
    let s = s.strip_prefix("dyn ").unwrap_or(s).trim_start();
    let end = s
        .char_indices()
        .find(|&(_, c)| !(c.is_alphanumeric() || c == '_' || c == ':'))
        .map_or(s.len(), |(i, _)| i);
    let path = &s[..end];
    (!path.is_empty() && !path.starts_with(':')).then_some((path, &s[end..]))
}

fn rules_for(lang: Lang) -> Option<&'static RuleSet> {
    static RUST: OnceLock<RuleSet> = OnceLock::new();
    static PYTHON: OnceLock<RuleSet> = OnceLock::new();
    static JS: OnceLock<RuleSet> = OnceLock::new();
    static GO: OnceLock<RuleSet> = OnceLock::new();
    static JVM: OnceLock<RuleSet> = OnceLock::new();
    static C: OnceLock<RuleSet> = OnceLock::new();
    static RUBY: OnceLock<RuleSet> = OnceLock::new();
    static PHP: OnceLock<RuleSet> = OnceLock::new();
    static LUA: OnceLock<RuleSet> = OnceLock::new();
    static SHELL: OnceLock<RuleSet> = OnceLock::new();
    static SQL: OnceLock<RuleSet> = OnceLock::new();
    let (cell, specs): (&'static OnceLock<RuleSet>, RuleSpecs) = match lang {
        Lang::Rust => (&RUST, rust_rules),
        Lang::Python => (&PYTHON, python_rules),
        Lang::JavaScript | Lang::TypeScript => (&JS, js_rules),
        Lang::Go => (&GO, go_rules),
        Lang::Java | Lang::Kotlin | Lang::Scala | Lang::CSharp | Lang::Swift => (&JVM, jvm_rules),
        Lang::C | Lang::Cpp => (&C, c_rules),
        Lang::Ruby => (&RUBY, ruby_rules),
        Lang::Php => (&PHP, php_rules),
        Lang::Lua => (&LUA, lua_rules),
        Lang::Shell => (&SHELL, shell_rules),
        Lang::Sql => (&SQL, sql_rules),
        _ => return None,
    };
    Some(cell.get_or_init(|| compile(specs())))
}

fn compile(specs: Vec<(&'static str, KindSpec)>) -> RuleSet {
    // Patterns are static and covered by tests; a failure here is a bug.
    let set = RegexSet::new(specs.iter().map(|(p, _)| *p)).expect("definition patterns compile");
    let rules = specs
        .into_iter()
        .map(|(p, kind)| Rule {
            re: Regex::new(p).expect("definition pattern compiles"),
            kind,
        })
        .collect();
    RuleSet { set, rules }
}

#[cfg(test)]
#[path = "defs_tests.rs"]
mod tests;
