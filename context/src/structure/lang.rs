//! Language detection and per-language lexical conventions.

use serde::{Deserialize, Serialize};

/// Source language of a document, detected from its path or content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Lang {
    /// Rust.
    Rust,
    /// Python.
    Python,
    /// JavaScript (including JSX).
    JavaScript,
    /// TypeScript (including TSX).
    TypeScript,
    /// Go.
    Go,
    /// Java.
    Java,
    /// Kotlin.
    Kotlin,
    /// Scala.
    Scala,
    /// C#.
    CSharp,
    /// C.
    C,
    /// C++.
    Cpp,
    /// Swift.
    Swift,
    /// Ruby.
    Ruby,
    /// PHP.
    Php,
    /// Lua.
    Lua,
    /// POSIX shell, Bash, Zsh.
    Shell,
    /// SQL.
    Sql,
    /// CSS / SCSS.
    Css,
    /// Markdown.
    Markdown,
    /// reStructuredText or AsciiDoc (segmented as plain text).
    Rst,
    /// Plain text.
    Text,
    /// HTML / XML (segmented as plain text).
    Html,
    /// JSON.
    Json,
    /// YAML.
    Yaml,
    /// TOML / INI.
    Toml,
}

/// How a language is segmented.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Family {
    /// Nesting follows `{` / `}`.
    Brace,
    /// Nesting follows indentation (also used for `end`-terminated languages,
    /// which are conventionally indented).
    Indent,
    /// Markdown headings and blocks.
    Markdown,
    /// Blank-line separated paragraphs.
    Plain,
}

impl Lang {
    /// Detect from a file name or path (extension, then well-known names).
    pub fn from_path(path: &str) -> Option<Lang> {
        let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
        let lower = name.to_ascii_lowercase();
        match lower.as_str() {
            "makefile" | "dockerfile" | "justfile" | ".bashrc" | ".zshrc" | ".profile" => {
                return Some(Lang::Shell)
            }
            "cargo.lock" | "pipfile" => return Some(Lang::Toml),
            "readme" | "license" | "copying" | "authors" | "changelog" => return Some(Lang::Text),
            _ => {}
        }
        let ext = lower.rsplit_once('.').map(|(_, e)| e)?;
        Some(match ext {
            "rs" => Lang::Rust,
            "py" | "pyi" | "pyw" => Lang::Python,
            "js" | "jsx" | "mjs" | "cjs" => Lang::JavaScript,
            "ts" | "tsx" | "mts" | "cts" => Lang::TypeScript,
            "go" => Lang::Go,
            "java" => Lang::Java,
            "kt" | "kts" => Lang::Kotlin,
            "scala" | "sc" => Lang::Scala,
            "cs" => Lang::CSharp,
            "c" | "h" => Lang::C,
            "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" | "ipp" | "m" | "mm" => Lang::Cpp,
            "swift" => Lang::Swift,
            "rb" | "rake" | "gemspec" => Lang::Ruby,
            "php" => Lang::Php,
            "lua" => Lang::Lua,
            "sh" | "bash" | "zsh" | "fish" | "ps1" | "mk" => Lang::Shell,
            "sql" => Lang::Sql,
            "css" | "scss" | "less" => Lang::Css,
            "md" | "markdown" | "mdx" | "mkd" => Lang::Markdown,
            "rst" | "adoc" | "asciidoc" | "org" | "tex" => Lang::Rst,
            "txt" | "text" | "log" => Lang::Text,
            "html" | "htm" | "xml" | "svg" | "xhtml" | "vue" | "svelte" => Lang::Html,
            "json" | "jsonc" | "json5" | "ipynb" => Lang::Json,
            "yaml" | "yml" => Lang::Yaml,
            "toml" | "ini" | "cfg" | "conf" | "properties" => Lang::Toml,
            _ => return None,
        })
    }

    /// Guess from content when the path gives no answer: a shebang, or
    /// Markdown headings/fences; otherwise plain text.
    pub fn sniff(text: &str) -> Lang {
        let first = text.lines().next().unwrap_or("");
        if let Some(shebang) = first.strip_prefix("#!") {
            if shebang.contains("python") {
                return Lang::Python;
            }
            if shebang.contains("node") {
                return Lang::JavaScript;
            }
            if shebang.contains("ruby") {
                return Lang::Ruby;
            }
            return Lang::Shell;
        }
        let markdownish = text.lines().take(200).any(|l| {
            let t = l.trim_start();
            t.starts_with("# ") || t.starts_with("## ") || t.starts_with("```")
        });
        if markdownish {
            Lang::Markdown
        } else {
            Lang::Text
        }
    }

    /// Detect from a path, falling back to content sniffing.
    pub fn detect(path: Option<&str>, text: &str) -> Lang {
        path.and_then(Lang::from_path)
            .unwrap_or_else(|| Lang::sniff(text))
    }

    /// Lowercase name, e.g. `"rust"`.
    pub fn as_str(self) -> &'static str {
        match self {
            Lang::Rust => "rust",
            Lang::Python => "python",
            Lang::JavaScript => "javascript",
            Lang::TypeScript => "typescript",
            Lang::Go => "go",
            Lang::Java => "java",
            Lang::Kotlin => "kotlin",
            Lang::Scala => "scala",
            Lang::CSharp => "csharp",
            Lang::C => "c",
            Lang::Cpp => "cpp",
            Lang::Swift => "swift",
            Lang::Ruby => "ruby",
            Lang::Php => "php",
            Lang::Lua => "lua",
            Lang::Shell => "shell",
            Lang::Sql => "sql",
            Lang::Css => "css",
            Lang::Markdown => "markdown",
            Lang::Rst => "rst",
            Lang::Text => "text",
            Lang::Html => "html",
            Lang::Json => "json",
            Lang::Yaml => "yaml",
            Lang::Toml => "toml",
        }
    }

    /// Stable on-disk code. Never renumber; only append.
    pub(crate) fn code(self) -> u8 {
        ALL.iter()
            .position(|&l| l == self)
            .map_or(0, |i| i as u8 + 1)
    }

    pub(crate) fn from_code(code: u8) -> Option<Lang> {
        let idx = (code as usize).checked_sub(1)?;
        ALL.get(idx).copied()
    }

    pub(crate) fn family(self) -> Family {
        match self {
            Lang::Rust
            | Lang::JavaScript
            | Lang::TypeScript
            | Lang::Go
            | Lang::Java
            | Lang::Kotlin
            | Lang::Scala
            | Lang::CSharp
            | Lang::C
            | Lang::Cpp
            | Lang::Swift
            | Lang::Php
            | Lang::Css
            | Lang::Json => Family::Brace,
            Lang::Python
            | Lang::Ruby
            | Lang::Lua
            | Lang::Shell
            | Lang::Sql
            | Lang::Yaml
            | Lang::Toml => Family::Indent,
            Lang::Markdown => Family::Markdown,
            Lang::Rst | Lang::Text | Lang::Html => Family::Plain,
        }
    }

    /// True for data formats, whose passages are tagged `Data`.
    pub(crate) fn is_data(self) -> bool {
        matches!(self, Lang::Json | Lang::Yaml | Lang::Toml)
    }

    /// Line-comment introducers.
    pub(crate) fn line_comments(self) -> &'static [&'static str] {
        match self {
            Lang::Python | Lang::Ruby | Lang::Shell | Lang::Yaml | Lang::Toml => &["#"],
            Lang::Php => &["//", "#"],
            Lang::Lua | Lang::Sql => &["--"],
            Lang::Css | Lang::Json | Lang::Markdown | Lang::Rst | Lang::Text | Lang::Html => &[],
            _ => &["//"],
        }
    }

    /// Whether `/* ... */` block comments exist.
    pub(crate) fn has_block_comments(self) -> bool {
        matches!(self.family(), Family::Brace) && self != Lang::Json
    }

    /// Whether `'` opens a string (as opposed to a char literal or a Rust
    /// lifetime).
    pub(crate) fn single_quote_strings(self) -> bool {
        matches!(
            self,
            Lang::JavaScript
                | Lang::TypeScript
                | Lang::Php
                | Lang::Python
                | Lang::Ruby
                | Lang::Lua
                | Lang::Shell
                | Lang::Sql
                | Lang::Css
        )
    }

    /// Whether backticks delimit (possibly multi-line) strings.
    pub(crate) fn backtick_strings(self) -> bool {
        matches!(self, Lang::JavaScript | Lang::TypeScript | Lang::Go)
    }

    /// Whether `"""` opens a multi-line string.
    pub(crate) fn triple_quote_strings(self) -> bool {
        matches!(
            self,
            Lang::Python | Lang::Kotlin | Lang::Scala | Lang::Swift | Lang::Java
        )
    }
}

/// Every language, in on-disk code order (code = index + 1).
const ALL: &[Lang] = &[
    Lang::Rust,
    Lang::Python,
    Lang::JavaScript,
    Lang::TypeScript,
    Lang::Go,
    Lang::Java,
    Lang::Kotlin,
    Lang::Scala,
    Lang::CSharp,
    Lang::C,
    Lang::Cpp,
    Lang::Swift,
    Lang::Ruby,
    Lang::Php,
    Lang::Lua,
    Lang::Shell,
    Lang::Sql,
    Lang::Css,
    Lang::Markdown,
    Lang::Rst,
    Lang::Text,
    Lang::Html,
    Lang::Json,
    Lang::Yaml,
    Lang::Toml,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_by_extension_and_name() {
        assert_eq!(Lang::from_path("src/lib.rs"), Some(Lang::Rust));
        assert_eq!(Lang::from_path("a/b/README.md"), Some(Lang::Markdown));
        assert_eq!(Lang::from_path("Makefile"), Some(Lang::Shell));
        assert_eq!(Lang::from_path("x\\y\\main.PY"), Some(Lang::Python));
        assert_eq!(Lang::from_path("noext"), None);
        assert_eq!(Lang::from_path("archive.tar.gz"), None);
    }

    #[test]
    fn sniffs_shebangs_and_markdown() {
        assert_eq!(
            Lang::sniff("#!/usr/bin/env python3\nprint(1)"),
            Lang::Python
        );
        assert_eq!(Lang::sniff("#!/bin/sh\necho hi"), Lang::Shell);
        assert_eq!(Lang::sniff("# Title\n\ntext"), Lang::Markdown);
        assert_eq!(Lang::sniff("just words"), Lang::Text);
        assert_eq!(Lang::detect(Some("x.go"), "# not markdown"), Lang::Go);
        assert_eq!(Lang::detect(None, "# Heading"), Lang::Markdown);
    }

    #[test]
    fn codes_round_trip_and_are_nonzero() {
        for &lang in ALL {
            let code = lang.code();
            assert_ne!(code, 0);
            assert_eq!(Lang::from_code(code), Some(lang));
        }
        assert_eq!(Lang::from_code(0), None);
        assert_eq!(Lang::from_code(250), None);
    }

    #[test]
    fn families() {
        assert_eq!(Lang::Rust.family(), Family::Brace);
        assert_eq!(Lang::Python.family(), Family::Indent);
        assert_eq!(Lang::Markdown.family(), Family::Markdown);
        assert_eq!(Lang::Text.family(), Family::Plain);
        assert!(Lang::Json.is_data() && !Lang::Go.is_data());
    }
}
