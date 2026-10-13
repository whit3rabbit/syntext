//! Per-language definition-line rules (see `defs`).

use super::defs::KindSpec;
use crate::types::SymbolKind;
use KindSpec::{Fixed, Guarded, Keyword};
use SymbolKind as K;

/// Prefix a Rust rule with the optional visibility qualifier.
macro_rules! rust_vis {
    ($body:literal) => {
        concat!(r"^\s*(?:pub(?:\s*\([^)]*\))?\s+)?", $body)
    };
}

pub(super) fn rust_rules() -> Vec<(&'static str, KindSpec)> {
    vec![
        (
            rust_vis!(
                r#"(?:default\s+)?(?:(?:const|async|unsafe|extern(?:\s+"[^"]*")?)\s+)*fn\s+([A-Za-z_]\w*)"#
            ),
            Fixed(K::Function),
        ),
        (rust_vis!(r"struct\s+([A-Za-z_]\w*)"), Fixed(K::Struct)),
        (rust_vis!(r"union\s+([A-Za-z_]\w*)"), Fixed(K::Struct)),
        (rust_vis!(r"enum\s+([A-Za-z_]\w*)"), Fixed(K::Enum)),
        (
            rust_vis!(r"(?:unsafe\s+)?(?:auto\s+)?trait\s+([A-Za-z_]\w*)"),
            Fixed(K::Interface),
        ),
        (rust_vis!(r"type\s+([A-Za-z_]\w*)"), Fixed(K::TypeAlias)),
        (rust_vis!(r"mod\s+([A-Za-z_]\w*)"), Fixed(K::Module)),
        (
            rust_vis!(r"(?:const|static)\s+(?:mut\s+)?([A-Za-z_]\w*)\s*:"),
            Fixed(K::Constant),
        ),
        (r"^\s*macro_rules!\s*([A-Za-z_]\w*)", Fixed(K::Macro)),
    ]
}

pub(super) fn python_rules() -> Vec<(&'static str, KindSpec)> {
    vec![
        (r"^\s*(?:async\s+)?def\s+([A-Za-z_]\w*)", Fixed(K::Function)),
        (r"^\s*class\s+([A-Za-z_]\w*)", Fixed(K::Class)),
    ]
}

pub(super) fn js_rules() -> Vec<(&'static str, KindSpec)> {
    vec![
        (
            r"^\s*(?:export\s+)?(?:default\s+)?(?:async\s+)?function\s*\*?\s*([A-Za-z_$][\w$]*)",
            Fixed(K::Function),
        ),
        (
            r"^\s*(?:export\s+)?(?:default\s+)?(?:abstract\s+)?class\s+([A-Za-z_$][\w$]*)",
            Fixed(K::Class),
        ),
        (
            r"^\s*(?:export\s+)?interface\s+([A-Za-z_$][\w$]*)",
            Fixed(K::Interface),
        ),
        (
            r"^\s*(?:export\s+)?(?:declare\s+)?(?:const\s+)?enum\s+([A-Za-z_$][\w$]*)",
            Fixed(K::Enum),
        ),
        (
            r"^\s*(?:export\s+)?type\s+([A-Za-z_$][\w$]*)\s*(?:<[^=]*>)?\s*=",
            Fixed(K::TypeAlias),
        ),
        (
            r"^\s*(?:export\s+)?(?:declare\s+)?namespace\s+([\w.]+)",
            Fixed(K::Module),
        ),
        (
            r"^\s*(?:export\s+)?(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*(?::[^=]+)?=\s*(?:async\s+)?(?:function\b|\([^)]*\)\s*(?::[^=]+)?=>|[A-Za-z_$][\w$]*\s*=>)",
            Fixed(K::Function),
        ),
        (
            r"^\s+(?:(?:public|private|protected|static|readonly|async|override|abstract|get|set)\s+)*([A-Za-z_$][\w$]*)\s*(?:<[^>]*>)?\s*\([^)]*\)\s*(?::\s*[^{]+)?\{\s*$",
            Guarded(K::Method),
        ),
    ]
}

pub(super) fn go_rules() -> Vec<(&'static str, KindSpec)> {
    vec![
        (r"^func\s+\([^)]*\)\s*([A-Za-z_]\w*)", Fixed(K::Method)),
        (r"^func\s+([A-Za-z_]\w*)", Fixed(K::Function)),
        (r"^type\s+([A-Za-z_]\w*)\s+struct\b", Fixed(K::Struct)),
        (r"^type\s+([A-Za-z_]\w*)\s+interface\b", Fixed(K::Interface)),
        (r"^type\s+([A-Za-z_]\w*)\b", Fixed(K::TypeAlias)),
        (r"^(?:const|var)\s+([A-Za-z_]\w*)", Fixed(K::Constant)),
    ]
}

pub(super) fn jvm_rules() -> Vec<(&'static str, KindSpec)> {
    vec![
        (
            r"^\s*(?:@\w+\s+)*(?:(?:public|private|protected|internal|static|final|abstract|sealed|open|data|inner|partial|case|implicit|override|fileprivate|readonly)\s+)*(class|interface|enum|record|struct|object|trait|protocol|extension)\s+([A-Za-z_]\w*)",
            Keyword,
        ),
        (
            r"^\s*(?:(?:public|private|protected|internal|override|open|suspend|inline|operator|infix|tailrec|abstract|final)\s+)*fun\s+(?:<[^>]*>\s*)?(?:[\w.]+\.)?([A-Za-z_]\w*)",
            Fixed(K::Function),
        ),
        (
            r"^\s*(?:(?:public|private|fileprivate|internal|open|static|class|override|mutating|final|@\w+)\s+)*func\s+([A-Za-z_]\w*)",
            Fixed(K::Function),
        ),
        (
            r"^\s*(?:(?:override|private|protected|final|implicit)\s+)*def\s+([A-Za-z_]\w*)",
            Fixed(K::Function),
        ),
        (
            r"^\s*(?:(?:public|private|protected|internal|static|final|abstract|synchronized|native|virtual|override|async|sealed|extern|unsafe|default)\s+)+[\w<>\[\],.?]+(?:\s+[\w<>\[\],.?]+)*?\s+([A-Za-z_]\w*)\s*\([^;]*$",
            Guarded(K::Method),
        ),
    ]
}

pub(super) fn c_rules() -> Vec<(&'static str, KindSpec)> {
    vec![
        (r"^\s*#\s*define\s+([A-Za-z_]\w*)", Fixed(K::Macro)),
        (
            r"^\s*(?:typedef\s+)?struct\s+([A-Za-z_]\w*)\s*(?:\{|$)",
            Fixed(K::Struct),
        ),
        (
            r"^\s*(?:typedef\s+)?union\s+([A-Za-z_]\w*)\s*(?:\{|$)",
            Fixed(K::Struct),
        ),
        (
            r"^\s*(?:typedef\s+)?enum\s+(?:class\s+)?([A-Za-z_]\w*)",
            Fixed(K::Enum),
        ),
        (
            r"^\s*(?:template\s*<.*>\s*)?class\s+([A-Za-z_]\w*)\s*(?:[:{]|$)",
            Fixed(K::Class),
        ),
        (r"^\s*namespace\s+([A-Za-z_]\w*)", Fixed(K::Module)),
        (
            r"^(?:[A-Za-z_][\w\s\*&:<>,]*?[\s\*&])?([A-Za-z_~][\w:~]*)\s*\([^;]*\)\s*(?:const\s*)?(?:noexcept\s*)?(?:override\s*)?(?:\{.*)?$",
            Guarded(K::Function),
        ),
    ]
}

pub(super) fn ruby_rules() -> Vec<(&'static str, KindSpec)> {
    vec![
        (
            r"^\s*def\s+(?:self\.)?([A-Za-z_]\w*[?!=]?)",
            Fixed(K::Function),
        ),
        (r"^\s*class\s+([A-Z]\w*(?:::[A-Z]\w*)*)", Fixed(K::Class)),
        (r"^\s*module\s+([A-Z]\w*(?:::[A-Z]\w*)*)", Fixed(K::Module)),
    ]
}

pub(super) fn php_rules() -> Vec<(&'static str, KindSpec)> {
    vec![
        (
            r"^\s*(?:(?:public|private|protected|static|abstract|final)\s+)*function\s+&?([A-Za-z_]\w*)",
            Fixed(K::Function),
        ),
        (
            r"^\s*(?:(?:abstract|final|readonly)\s+)*class\s+([A-Za-z_]\w*)",
            Fixed(K::Class),
        ),
        (r"^\s*interface\s+([A-Za-z_]\w*)", Fixed(K::Interface)),
        (r"^\s*trait\s+([A-Za-z_]\w*)", Fixed(K::Interface)),
    ]
}

pub(super) fn lua_rules() -> Vec<(&'static str, KindSpec)> {
    vec![
        (r"^\s*(?:local\s+)?function\s+([\w.:]+)", Fixed(K::Function)),
        (
            r"^\s*(?:local\s+)?([A-Za-z_][\w.]*)\s*=\s*function\b",
            Fixed(K::Function),
        ),
    ]
}

pub(super) fn shell_rules() -> Vec<(&'static str, KindSpec)> {
    vec![
        (r"^\s*function\s+([A-Za-z_][\w-]*)", Fixed(K::Function)),
        (
            r"^\s*([A-Za-z_][\w-]*)\s*\(\)\s*\{?\s*$",
            Fixed(K::Function),
        ),
    ]
}

pub(super) fn sql_rules() -> Vec<(&'static str, KindSpec)> {
    vec![(
        r#"(?i)^\s*create\s+(?:or\s+replace\s+)?(?:temporary\s+|temp\s+|unique\s+)?(table|view|function|procedure|index|trigger|type)\s+(?:if\s+not\s+exists\s+)?([\w."]+)"#,
        Keyword,
    )]
}
