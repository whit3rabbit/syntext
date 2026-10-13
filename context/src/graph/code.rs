//! Code facts: definitions, containment, inheritance, calls, import aliases.

use std::sync::OnceLock;

use regex::Regex;

use super::{entity, AliasFact, EdgeFact, EdgeKind, EntityKind, EntityRef, PassageFacts};
use crate::structure::{code_line_flags, detect_def, is_comment_line, Lang, PassageSpan};
use crate::types::SymbolKind;

/// Distinct callees recorded per passage.
const MAX_CALLS: usize = 24;

/// Names followed by `(` that are keywords, builtins, or ubiquitous helpers;
/// edges to them would only add hubs. Sorted for binary search.
const NOT_CALLEES: &[&str] = &[
    "Arc",
    "Box",
    "Err",
    "Mutex",
    "Ok",
    "Option",
    "Rc",
    "RefCell",
    "Result",
    "Some",
    "String",
    "Vec",
    "and",
    "assert",
    "assert_eq",
    "assert_ne",
    "await",
    "case",
    "catch",
    "clone",
    "collect",
    "console",
    "def",
    "defer",
    "dict",
    "elif",
    "else",
    "eprintln",
    "except",
    "expect",
    "fn",
    "for",
    "foreach",
    "format",
    "func",
    "function",
    "if",
    "in",
    "int",
    "into",
    "is",
    "isinstance",
    "iter",
    "lambda",
    "len",
    "list",
    "lock",
    "loop",
    "map",
    "match",
    "new",
    "not",
    "or",
    "panic",
    "print",
    "printf",
    "println",
    "push",
    "raise",
    "range",
    "require",
    "return",
    "select",
    "self",
    "set",
    "sizeof",
    "str",
    "super",
    "switch",
    "this",
    "throw",
    "to_owned",
    "to_string",
    "try",
    "tuple",
    "typeof",
    "unless",
    "until",
    "unwrap",
    "using",
    "when",
    "while",
    "with",
    "write",
    "writeln",
    "yield",
];

pub(super) fn extract(body: &str, span: &PassageSpan, lang: Lang, facts: &mut PassageFacts) {
    let callable = |kind: SymbolKind| matches!(kind, SymbolKind::Function | SymbolKind::Method);
    // Calls before the first definition line belong to the passage's own
    // function, or to the function it was split out of.
    let mut caller: Option<EntityRef> = span
        .symbol
        .as_ref()
        .filter(|s| callable(s.kind))
        .or_else(|| span.container.as_ref().filter(|c| callable(c.kind)))
        .and_then(|s| entity(&s.name, EntityKind::Symbol));
    let mut defined: Vec<(EntityRef, SymbolKind)> = Vec::new();
    let mut calls: Vec<(EntityRef, EntityRef)> = Vec::new();
    let code_lines = code_line_flags(body, lang);
    for (i, line) in body.lines().enumerate() {
        let t = line.trim_start();
        let in_literal = !code_lines.get(i).copied().unwrap_or(true);
        if t.is_empty() || in_literal || is_comment_line(t, lang) {
            continue;
        }
        if let Some(def) = detect_def(line, lang) {
            if let Some(e) = entity(&def.name, EntityKind::Symbol) {
                if let Some(tr) = def
                    .implements
                    .as_deref()
                    .and_then(|t| entity(t, EntityKind::Symbol))
                {
                    push_edge(facts, &e, "implements", &tr, EdgeKind::Implements, 0.95);
                }
                caller = callable(def.kind).then(|| e.clone());
                defined.push((e, def.kind));
            }
        }
        if let Some(c) = &caller {
            collect_calls(line, c, &mut calls);
        }
        inheritance(line, lang, facts);
        import_aliases(line, lang, facts);
    }

    for (e, kind) in &defined {
        if *kind == SymbolKind::Impl {
            // An impl block implements its type; it does not define it.
            facts.mentions.push(e.clone());
        } else {
            facts.defines.push(e.clone());
        }
    }
    let container = span
        .container
        .as_ref()
        .filter(|c| !callable(c.kind))
        .and_then(|c| entity(&c.name, EntityKind::Symbol));
    if let Some(container) = container {
        for (e, kind) in &defined {
            if *kind != SymbolKind::Impl && e.key != container.key {
                push_edge(facts, e, "member of", &container, EdgeKind::MemberOf, 1.0);
            }
        }
    }
    for (from, to) in calls {
        if !defined.iter().any(|(d, _)| d.key == to.key) {
            push_edge(facts, &from, "calls", &to, EdgeKind::Calls, 0.6);
        }
    }
}

fn push_edge(
    facts: &mut PassageFacts,
    subject: &EntityRef,
    predicate: &str,
    object: &EntityRef,
    kind: EdgeKind,
    confidence: f32,
) {
    if subject.key == object.key {
        return;
    }
    facts.edges.push(EdgeFact {
        subject: subject.clone(),
        predicate: predicate.to_string(),
        object: object.clone(),
        kind,
        confidence,
    });
}

/// Record `caller -> name` for every `name(` on `line`, up to
/// [`MAX_CALLS`] distinct pairs per passage.
fn collect_calls(line: &str, caller: &EntityRef, out: &mut Vec<(EntityRef, EntityRef)>) {
    static CALL: OnceLock<Regex> = OnceLock::new();
    let re = CALL.get_or_init(|| {
        Regex::new(r"\b([A-Za-z_][A-Za-z0-9_]*)\s*(?:::<[^>]*>)?\s*\(").expect("call pattern")
    });
    for caps in re.captures_iter(line) {
        if out.len() >= MAX_CALLS {
            return;
        }
        let name = &caps[1];
        if name.len() < 3 || NOT_CALLEES.binary_search(&name).is_ok() {
            continue;
        }
        let Some(callee) = entity(name, EntityKind::Symbol) else {
            continue;
        };
        let dup = out
            .iter()
            .any(|(f, t)| f.key == caller.key && t.key == callee.key);
        if callee.key != caller.key && !dup {
            out.push((caller.clone(), callee));
        }
    }
}

fn inheritance(line: &str, lang: Lang, facts: &mut PassageFacts) {
    static PY: OnceLock<Regex> = OnceLock::new();
    static EXTENDS: OnceLock<Regex> = OnceLock::new();
    static IMPLEMENTS: OnceLock<Regex> = OnceLock::new();
    static COLON: OnceLock<Regex> = OnceLock::new();
    static TRAIT: OnceLock<Regex> = OnceLock::new();
    let (re, kind) = match lang {
        Lang::Python => (
            PY.get_or_init(|| Regex::new(r"^\s*class\s+(\w+)\s*\(([^)]*)\)").expect("py bases")),
            EdgeKind::Extends,
        ),
        Lang::Rust => (
            TRAIT.get_or_init(|| {
                Regex::new(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:unsafe\s+)?trait\s+(\w+)(?:<[^>]*>)?\s*:\s*([\w\s+:<>']+?)\s*(?:\{|where|$)")
                    .expect("rust supertraits")
            }),
            EdgeKind::Extends,
        ),
        Lang::Kotlin | Lang::Swift | Lang::CSharp | Lang::Cpp => (
            COLON.get_or_init(|| {
                Regex::new(r"(?:class|struct|interface|protocol|object)\s+(\w+)(?:<[^>]*>)?(?:\([^)]*\))?\s*:\s*([\w\s.,<>()]+?)\s*\{?\s*$")
                    .expect("colon bases")
            }),
            EdgeKind::Extends,
        ),
        _ => {
            let ext = EXTENDS.get_or_init(|| {
                Regex::new(r"class\s+(\w+)(?:<[^>]*>)?\s+extends\s+([\w.]+)").expect("extends")
            });
            if let Some(c) = ext.captures(line) {
                add_bases(&c[1], &c[2], EdgeKind::Extends, facts);
            }
            (
                IMPLEMENTS.get_or_init(|| {
                    Regex::new(r"(?:class|enum|record)\s+(\w+).*?\bimplements\s+([\w.,\s<>]+?)\s*\{?\s*$")
                        .expect("implements")
                }),
                EdgeKind::Implements,
            )
        }
    };
    if let Some(c) = re.captures(line) {
        add_bases(&c[1], &c[2], kind, facts);
    }
}

fn add_bases(child: &str, bases: &str, kind: EdgeKind, facts: &mut PassageFacts) {
    let Some(child) = entity(child, EntityKind::Symbol) else {
        return;
    };
    let predicate = if kind == EdgeKind::Implements {
        "implements"
    } else {
        "extends"
    };
    for raw in bases.split([',', '+']) {
        let base = raw
            .split('<')
            .next()
            .unwrap_or("")
            .split_whitespace()
            .rfind(|w| !matches!(*w, "public" | "private" | "protected" | "virtual"))
            .unwrap_or("");
        let base = base.trim_end_matches("()");
        if base.is_empty() || base.contains('=') || matches!(base, "object" | "'static") {
            continue;
        }
        if let Some(b) = entity(
            base.rsplit(['.', ':']).next().unwrap_or(base),
            EntityKind::Symbol,
        ) {
            push_edge(facts, &child, predicate, &b, kind, 0.9);
        }
    }
}

fn import_aliases(line: &str, lang: Lang, facts: &mut PassageFacts) {
    static RUST_USE: OnceLock<Regex> = OnceLock::new();
    static RUST_TYPE: OnceLock<Regex> = OnceLock::new();
    static PY_IMPORT: OnceLock<Regex> = OnceLock::new();
    static JS_STAR: OnceLock<Regex> = OnceLock::new();
    static JS_NAMED: OnceLock<Regex> = OnceLock::new();
    static TS_TYPE: OnceLock<Regex> = OnceLock::new();
    static GO_IMPORT: OnceLock<Regex> = OnceLock::new();
    let mut pairs: Vec<(String, String, f32)> = Vec::new();
    match lang {
        Lang::Rust => {
            let re = RUST_USE.get_or_init(|| Regex::new(r"\b(\w+)\s+as\s+(\w+)").expect("use as"));
            if line.trim_start().starts_with("use ") || line.contains("pub use ") {
                for c in re.captures_iter(line) {
                    pairs.push((c[2].to_string(), c[1].to_string(), 1.0));
                }
            }
            let re = RUST_TYPE.get_or_init(|| {
                Regex::new(r"^\s*(?:pub(?:\([^)]*\))?\s+)?type\s+(\w+)(?:<[^>]*>)?\s*=\s*([\w:]+)")
                    .expect("type alias")
            });
            if let Some(c) = re.captures(line) {
                pairs.push((c[1].to_string(), last(&c[2]).to_string(), 0.8));
            }
        }
        Lang::Python => {
            let re =
                PY_IMPORT.get_or_init(|| Regex::new(r"\b([\w.]+)\s+as\s+(\w+)").expect("py as"));
            let t = line.trim_start();
            if t.starts_with("import ") || t.starts_with("from ") {
                for c in re.captures_iter(line) {
                    pairs.push((c[2].to_string(), last(&c[1]).to_string(), 1.0));
                }
            }
        }
        Lang::JavaScript | Lang::TypeScript => {
            let star = JS_STAR.get_or_init(|| {
                Regex::new(r#"import\s+\*\s+as\s+(\w+)\s+from\s+['"]([^'"]+)['"]"#)
                    .expect("js star")
            });
            if let Some(c) = star.captures(line) {
                pairs.push((c[1].to_string(), last(&c[2]).to_string(), 0.9));
            }
            let named =
                JS_NAMED.get_or_init(|| Regex::new(r"\b(\w+)\s+as\s+(\w+)").expect("js as"));
            if line.contains('{') && line.trim_start().starts_with("import") {
                for c in named.captures_iter(line) {
                    pairs.push((c[2].to_string(), c[1].to_string(), 1.0));
                }
            }
            let ty = TS_TYPE.get_or_init(|| {
                Regex::new(r"^\s*(?:export\s+)?type\s+(\w+)\s*=\s*([\w.]+)\s*;?\s*$")
                    .expect("ts type")
            });
            if let Some(c) = ty.captures(line) {
                pairs.push((c[1].to_string(), last(&c[2]).to_string(), 0.8));
            }
        }
        Lang::Go => {
            let re = GO_IMPORT.get_or_init(|| {
                Regex::new(r#"^\s*(?:import\s+)?(\w+)\s+"([^"]+)"\s*$"#).expect("go import")
            });
            if let Some(c) = re.captures(line) {
                pairs.push((c[1].to_string(), last(&c[2]).to_string(), 0.9));
            }
        }
        _ => {}
    }
    for (alias, target, confidence) in pairs {
        let (Some(a), Some(t)) = (
            entity(&alias, EntityKind::Symbol),
            entity(&target, EntityKind::Symbol),
        ) else {
            continue;
        };
        if a.key != t.key {
            facts.aliases.push(AliasFact {
                alias: a.key,
                target: t.key,
                confidence,
            });
        }
    }
}

/// Last segment of a module path (`a::b::C`, `a.b.c`, `pkg/sub`).
fn last(path: &str) -> &str {
    path.rsplit([':', '.', '/'])
        .find(|s| !s.is_empty())
        .unwrap_or(path)
}

#[cfg(test)]
pub(super) fn callee_stoplist() -> &'static [&'static str] {
    NOT_CALLEES
}
