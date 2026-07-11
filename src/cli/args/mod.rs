//! CLI argument definitions: `Cli` struct and `ManageCommand` enum.
//!
//! Separated from `mod.rs` to keep flag definitions distinct from dispatch logic.

use std::path::PathBuf;

use clap::Parser;

pub use super::commands::ManageCommand;

mod compat;
pub use compat::CompatibilityArgs;
mod globs;

/// Fast code search with index acceleration. ripgrep-style interface.
///
/// Use `st index` to build the index first, then `st <pattern>` to search.
#[derive(Parser)]
#[command(name = "st", version, about, disable_help_subcommand = true)]
pub struct Cli {
    /// Pattern to search (regex by default). Use -F for literal, -e to avoid
    /// subcommand name conflicts (`st -e index`), or `--` to search for a
    /// colliding word (`st -- index` searches "index", not the rebuild).
    pub pattern: Option<String>,

    /// Paths (files or directories) to restrict the search.
    #[arg(value_name = "PATH")]
    pub paths: Vec<PathBuf>,

    // --- Match options ---
    /// Treat PATTERN as a literal string (not a regex). Equivalent to rg -F.
    #[arg(short = 'F', long = "fixed-strings")]
    pub fixed_strings: bool,

    /// Execute the search case sensitively (the default).
    #[arg(short = 's', long = "case-sensitive", overrides_with_all = ["ignore_case", "smart_case"])]
    pub case_sensitive: bool,

    /// Case-insensitive search.
    #[arg(short = 'i', long = "ignore-case", overrides_with_all = ["case_sensitive", "smart_case"])]
    pub ignore_case: bool,

    /// Case-insensitive if pattern is all lowercase, case-sensitive otherwise.
    /// Note: uppercase regex class shorthands (\S, \D, \W) and ranges like
    /// [A-Z] count as uppercase and force case-sensitive mode.
    #[arg(short = 'S', long = "smart-case", overrides_with_all = ["ignore_case", "case_sensitive"])]
    pub smart_case: bool,

    /// Only match whole words (wraps pattern in \b...\b).
    #[arg(short = 'w', long = "word-regexp", overrides_with = "line_regexp")]
    pub word_regexp: bool,

    /// Only match lines where the entire line participates in a match.
    #[arg(short = 'x', long = "line-regexp", overrides_with = "word_regexp")]
    pub line_regexp: bool,

    /// Invert matching: print lines that do NOT match the pattern.
    /// Scans every file in scope (like grep -v and rg -v), not just index
    /// candidates, so files containing no match still contribute their lines.
    #[arg(short = 'v', long = "invert-match")]
    pub invert_match: bool,

    /// Look up a symbol by name (prefix match) via the symbol index instead of
    /// searching file contents. Replaces the old `sym:`/`def:`/`ref:` pattern
    /// prefixes so those strings can be searched literally again.
    #[cfg(feature = "symbols")]
    #[arg(long = "sym", value_name = "NAME")]
    pub sym: Option<String>,

    /// Restrict `--sym` (or `--refs`) to a symbol kind (function, method, class,
    /// struct, enum, trait, interface, const, type). Ignored without a name flag.
    #[cfg(feature = "symbols")]
    #[arg(long = "sym-kind", value_name = "KIND")]
    pub sym_kind: Option<String>,

    /// Find references to NAME: resolve NAME via the symbol index, then search
    /// for word-boundary, case-sensitive occurrences across the corpus. Not
    /// scope-aware (matches shadowed identifiers, strings, comments). Requires
    /// the symbols feature.
    #[cfg(feature = "symbols")]
    #[arg(long = "refs", value_name = "NAME")]
    pub refs: Option<String>,

    /// Specify pattern explicitly (avoids subcommand name conflicts).
    /// Can be given multiple times; patterns are combined with OR (|).
    #[arg(
        short = 'e',
        long = "regexp",
        value_name = "PATTERN",
        action = clap::ArgAction::Append,
        allow_hyphen_values = true,
    )]
    pub regexp: Vec<String>,

    // --- Output format ---
    /// Print only paths of files with at least one match.
    #[arg(
        short = 'l',
        long = "files-with-matches",
        overrides_with_all = ["count", "json", "files_without_match"]
    )]
    pub files_with_matches: bool,

    /// Print only paths of files with zero matches.
    #[arg(
        long = "files-without-match",
        overrides_with_all = ["files_with_matches", "count", "count_matches", "json"]
    )]
    pub files_without_match: bool,

    /// Print count of matching lines per file.
    #[arg(
        short = 'c',
        long = "count",
        overrides_with_all = ["files_with_matches", "files_without_match", "count_matches", "json"]
    )]
    pub count: bool,

    /// Print count of individual matches per file.
    #[arg(
        long = "count-matches",
        overrides_with_all = ["files_with_matches", "files_without_match", "count", "json"]
    )]
    pub count_matches: bool,

    /// Limit the number of matching lines printed per file.
    #[arg(short = 'm', long = "max-count", value_name = "NUM")]
    pub max_count: Option<usize>,

    /// Suppress output; exit 0 if any match found.
    #[arg(short = 'q', long = "quiet")]
    pub quiet: bool,

    /// Print only the matched (non-empty) parts of matching lines.
    #[arg(short = 'o', long = "only-matching")]
    pub only_matching: bool,

    /// Output as NDJSON (ripgrep-style format).
    #[arg(long = "json", overrides_with_all = ["files_with_matches", "files_without_match", "count", "count_matches"])]
    pub json: bool,

    /// Group matches under their file name (like rg default on a tty).
    #[arg(long = "heading", overrides_with = "no_heading")]
    pub heading: bool,

    /// Print path:line:content on each line (default; overrides --heading).
    #[arg(long = "no-heading", overrides_with = "heading")]
    pub no_heading: bool,

    /// Show line numbers.
    #[arg(
        short = 'n',
        long = "line-number",
        action = clap::ArgAction::Count,
        overrides_with = "no_line_number"
    )]
    pub line_number: u8,

    /// Suppress line numbers in output.
    #[arg(
        short = 'N',
        long = "no-line-number",
        action = clap::ArgAction::Count,
        overrides_with = "line_number"
    )]
    pub no_line_number: u8,

    /// Show file names with matches.
    #[arg(short = 'H', long = "with-filename", overrides_with = "no_filename")]
    pub with_filename: bool,

    /// Suppress file names in output.
    #[arg(short = 'I', long = "no-filename", overrides_with = "with_filename")]
    pub no_filename: bool,

    /// Show 1-based column number of each match.
    #[arg(long)]
    pub column: bool,

    /// Output one match per line in path:line:column:content format.
    #[arg(long)]
    pub vimgrep: bool,

    /// Replace matches with the given text in output (not in files).
    /// Uses regex capture group syntax ($1, $2, etc.).
    #[arg(short = 'r', long = "replace", value_name = "REPLACEMENT")]
    pub replace: Option<String>,

    /// Follow file paths with NUL byte instead of newline.
    #[arg(short = '0', long = "null")]
    pub null: bool,

    /// Alias for --color=always --heading --line-number.
    #[arg(short = 'p', long = "pretty")]
    pub pretty: bool,

    // --- Context lines ---
    /// Show NUM lines after each match.
    #[arg(short = 'A', long = "after-context", value_name = "NUM")]
    pub after_context: Option<usize>,

    /// Show NUM lines before each match.
    #[arg(short = 'B', long = "before-context", value_name = "NUM")]
    pub before_context: Option<usize>,

    /// Show NUM lines before and after each match (sets -A and -B).
    #[arg(short = 'C', long = "context", value_name = "NUM")]
    pub context: Option<usize>,

    /// String to print between non-contiguous context blocks.
    #[arg(
        long = "context-separator",
        value_name = "SEPARATOR",
        default_value = "--"
    )]
    pub context_separator: String,

    // --- Filtering ---
    /// Restrict to file type extension (e.g. rs, py, js).
    #[arg(
        short = 't',
        long = "type",
        value_name = "TYPE",
        action = clap::ArgAction::Append
    )]
    pub file_type: Vec<String>,

    /// Exclude file type extension.
    #[arg(
        short = 'T',
        long = "type-not",
        value_name = "TYPE",
        action = clap::ArgAction::Append
    )]
    pub type_not: Vec<String>,

    /// Restrict to paths matching GLOB (e.g. "*.rs" or "src/**").
    #[arg(
        short = 'g',
        long = "glob",
        value_name = "GLOB",
        action = clap::ArgAction::Append
    )]
    pub glob: Vec<String>,

    /// Include paths matching GLOB (grep compatibility alias for --glob).
    #[arg(long = "include", value_name = "GLOB", action = clap::ArgAction::Append)]
    pub include: Vec<String>,

    /// Exclude paths matching GLOB (grep compatibility alias for --glob '!GLOB').
    #[arg(long = "exclude", value_name = "GLOB", action = clap::ArgAction::Append)]
    pub exclude: Vec<String>,

    /// Show all supported file types and their extensions.
    #[arg(long = "type-list")]
    pub type_list: bool,

    /// Print files that would be searched (no searching).
    #[arg(long)]
    pub files: bool,

    // --- Display ---
    /// Color output: always, never, or auto (default: auto, color only on a TTY).
    #[arg(
        long = "color",
        value_name = "WHEN",
        default_value = None,
        value_parser = ["always", "never", "auto", "ansi"]
    )]
    pub color: Option<String>,

    /// Custom color specs (rg compatibility): accepted but not yet honored;
    /// fixed defaults (match/path/line) are used.
    #[arg(long = "colors", value_name = "SPEC", action = clap::ArgAction::Append)]
    pub colors: Vec<String>,

    /// Show 0-based byte offset before each output line.
    #[arg(short = 'b', long = "byte-offset")]
    pub byte_offset: bool,

    /// Omit lines longer than NUM bytes.
    #[arg(short = 'M', long = "max-columns", value_name = "NUM")]
    pub max_columns: Option<usize>,

    /// Remove leading ASCII whitespace from each line.
    #[arg(long)]
    pub trim: bool,

    /// Print search statistics to stderr.
    #[arg(long = "stats")]
    pub search_stats: bool,

    /// Limit directory traversal depth.
    #[arg(short = 'd', long = "max-depth", value_name = "NUM")]
    pub max_depth: Option<usize>,

    /// No-op or compatibility options to mirror ripgrep interface.
    #[command(flatten)]
    pub compat: CompatibilityArgs,

    // --- Index configuration ---
    /// Override index directory (default: .syntext/ at repo root).
    #[arg(long, alias = "index", global = true, env = "SYNTEXT_INDEX_DIR")]
    pub index_dir: Option<PathBuf>,

    /// Override repository root (default: nearest .git ancestor).
    #[arg(long, global = true)]
    pub repo_root: Option<PathBuf>,

    /// Emit progress and diagnostic messages.
    #[arg(long, global = true)]
    pub verbose: bool,

    /// On a missing index, fall back to ripgrep (or grep) instead of erroring.
    /// Also enabled by SYNTEXT_FALLBACK_RG=1. Slower and lower-fidelity than the
    /// index; intended for searching un-indexed paths.
    #[arg(long = "fallback", global = true)]
    pub fallback: bool,

    /// Skip auto-update of the index before searching. Also disabled by
    /// SYNTEXT_NO_AUTO_UPDATE=1.
    #[arg(long = "no-update", global = true)]
    pub no_update: bool,

    /// Management subcommands (index, update, status).
    #[command(subcommand)]
    pub command: Option<ManageCommand>,
}


