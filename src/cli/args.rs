//! CLI argument definitions: `Cli` struct and `ManageCommand` enum.
//!
//! Separated from `mod.rs` to keep flag definitions distinct from dispatch logic.

use std::path::PathBuf;

use clap::Parser;

pub use super::commands::ManageCommand;

/// Fast code search with index acceleration. ripgrep-style interface.
///
/// Use `st index` to build the index first, then `st <pattern>` to search.
#[derive(Parser)]
#[command(name = "st", version, about, disable_help_subcommand = true)]
pub struct Cli {
    /// Pattern to search (regex by default). Use -F for literal, -e to avoid
    /// subcommand name conflicts (e.g. `st -e index`).
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
    #[arg(short = 'S', long = "smart-case", overrides_with_all = ["ignore_case", "case_sensitive"])]
    pub smart_case: bool,

    /// Only match whole words (wraps pattern in \b...\b).
    #[arg(short = 'w', long = "word-regexp", overrides_with = "line_regexp")]
    pub word_regexp: bool,

    /// Only match lines where the entire line participates in a match.
    #[arg(short = 'x', long = "line-regexp", overrides_with = "word_regexp")]
    pub line_regexp: bool,

    /// Invert matching: print lines that do NOT match the pattern within candidate files.
    /// Unlike grep -v and rg -v, this only examines files identified by the index as containing
    /// the pattern; non-candidate files are not searched (v1 limitation).
    #[arg(short = 'v', long = "invert-match")]
    pub invert_match: bool,

    /// Specify pattern explicitly (avoids subcommand name conflicts).
    /// Can be given multiple times; patterns are combined with OR (|).
    #[arg(
        short = 'e',
        long = "regexp",
        value_name = "PATTERN",
        action = clap::ArgAction::Append,
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
    #[arg(short = 'n', long = "line-number", overrides_with = "no_line_number")]
    pub line_number: bool,

    /// Suppress line numbers in output.
    #[arg(short = 'N', long = "no-line-number", overrides_with = "line_number")]
    pub no_line_number: bool,

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
    #[arg(short = 't', long = "type", value_name = "TYPE")]
    pub file_type: Option<String>,

    /// Exclude file type extension.
    #[arg(short = 'T', long = "type-not", value_name = "TYPE")]
    pub type_not: Option<String>,

    /// Restrict to paths matching GLOB (e.g. "*.rs" or "src/**").
    #[arg(short = 'g', long = "glob", value_name = "GLOB")]
    pub glob: Option<String>,

    /// Show all supported file types and their extensions.
    #[arg(long = "type-list")]
    pub type_list: bool,

    /// Print files that would be searched (no searching).
    #[arg(long)]
    pub files: bool,

    // --- Display ---
    /// Control color output (accepted for rg compatibility, currently no-op).
    #[arg(long = "color", value_name = "WHEN", default_value = None)]
    pub color: Option<String>,

    /// Custom color specifications (rg compatibility, no-op).
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

    // --- File discovery (mostly no-ops for indexed search) ---
    /// Search hidden files and directories (already default for indexed search).
    #[arg(long = "hidden")]
    pub hidden: bool,

    /// Don't respect ignore files (no-op for indexed search).
    #[arg(long = "no-ignore")]
    pub no_ignore: bool,

    /// Don't respect VCS ignore files (no-op for indexed search).
    #[arg(long = "no-ignore-vcs")]
    pub no_ignore_vcs: bool,

    /// Don't respect ignore files from parent directories (no-op).
    #[arg(long = "no-ignore-parent")]
    pub no_ignore_parent: bool,

    /// Additional ignore file path (no-op for indexed search).
    #[arg(long = "ignore-file", value_name = "PATH")]
    pub ignore_file: Option<PathBuf>,

    /// Follow symbolic links (no-op for indexed search).
    #[arg(short = 'L', long = "follow")]
    pub follow: bool,

    /// Limit directory traversal depth.
    #[arg(short = 'd', long = "max-depth", value_name = "NUM")]
    pub max_depth: Option<usize>,

    /// Reduce filtering: -u hidden, -uu no-ignore, -uuu binary (no-op).
    #[arg(short = 'u', long = "unrestricted", action = clap::ArgAction::Count)]
    pub unrestricted: u8,

    /// Don't cross filesystem boundaries (no-op).
    #[arg(long = "one-file-system")]
    pub one_file_system: bool,

    // --- Sorting ---
    /// Sort results by criterion: path, modified, accessed, created, none.
    #[arg(long = "sort", value_name = "SORTBY")]
    pub sort: Option<String>,

    /// Sort results in reverse order.
    #[arg(long = "sortr", value_name = "SORTBY")]
    pub sortr: Option<String>,

    // --- Binary/encoding ---
    /// Search binary files as if they were text (no-op for indexed search).
    #[arg(short = 'a', long = "text")]
    pub text: bool,

    /// Search binary files (no-op for indexed search).
    #[arg(long = "binary")]
    pub binary: bool,

    /// Ignore files larger than NUM+SUFFIX (no-op for indexed search).
    #[arg(long = "max-filesize", value_name = "NUM+SUFFIX")]
    pub max_filesize: Option<String>,

    /// Specify text encoding (no-op).
    #[arg(short = 'E', long = "encoding", value_name = "ENCODING")]
    pub encoding: Option<String>,

    /// Treat CRLF as line terminator (no-op).
    #[arg(long)]
    pub crlf: bool,

    /// Use NUL as line terminator (no-op).
    #[arg(long = "null-data")]
    pub null_data: bool,

    // --- Regex engine ---
    /// Use PCRE2 regex engine (not supported, warns).
    #[arg(short = 'P', long = "pcre2")]
    pub pcre2: bool,

    /// Enable multiline matching (no-op).
    #[arg(short = 'U', long = "multiline")]
    pub multiline: bool,

    /// Make dot match newlines in multiline mode (no-op).
    #[arg(long = "multiline-dotall")]
    pub multiline_dotall: bool,

    /// Choose regex engine (no-op).
    #[arg(long = "engine", value_name = "ENGINE")]
    pub engine: Option<String>,

    /// Set DFA size limit (no-op).
    #[arg(long = "dfa-size-limit", value_name = "NUM")]
    pub dfa_size_limit: Option<String>,

    /// Set regex compilation size limit (no-op).
    #[arg(long = "regex-size-limit", value_name = "NUM")]
    pub regex_size_limit: Option<String>,

    // --- Pattern source ---
    /// Read patterns from file (no-op).
    #[arg(short = 'f', long = "file", value_name = "PATTERNFILE")]
    pub pattern_file: Option<PathBuf>,

    // --- Type management ---
    /// Add a custom file type definition (no-op).
    #[arg(long = "type-add", value_name = "TYPESPEC", action = clap::ArgAction::Append)]
    pub type_add: Vec<String>,

    /// Clear file type definitions (no-op).
    #[arg(long = "type-clear", value_name = "TYPE", action = clap::ArgAction::Append)]
    pub type_clear: Vec<String>,

    /// Case-insensitive glob (no-op).
    #[arg(long = "iglob", value_name = "GLOB")]
    pub iglob: Option<String>,

    // --- Performance (no-ops) ---
    /// Number of threads (no-op).
    #[arg(short = 'j', long = "threads", value_name = "NUM")]
    pub threads: Option<usize>,

    /// Use memory maps (no-op).
    #[arg(long)]
    pub mmap: bool,

    /// Disable memory maps (no-op).
    #[arg(long = "no-mmap")]
    pub no_mmap: bool,

    // --- Preprocessing (no-ops) ---
    /// Preprocess files with command (no-op).
    #[arg(long = "pre", value_name = "COMMAND")]
    pub pre: Option<String>,

    /// Only preprocess files matching glob (no-op).
    #[arg(long = "pre-glob", value_name = "GLOB")]
    pub pre_glob: Option<String>,

    // --- Compressed files ---
    /// Search in compressed files (no-op).
    #[arg(short = 'z', long = "search-zip")]
    pub search_zip: bool,

    // --- Diagnostics ---
    /// Show debug messages (alias for --verbose).
    #[arg(long)]
    pub debug: bool,

    /// Show trace messages (no-op).
    #[arg(long)]
    pub trace: bool,

    /// Suppress error messages (no-op).
    #[arg(long = "no-messages")]
    pub no_messages: bool,

    /// Ignore configuration files (no-op).
    #[arg(long = "no-config")]
    pub no_config: bool,

    // --- Index configuration ---
    /// Override index directory (default: .syntext/ at repo root).
    #[arg(long, global = true, env = "SYNTEXT_INDEX_DIR")]
    pub index_dir: Option<PathBuf>,

    /// Override repository root (default: nearest .git ancestor).
    #[arg(long, global = true)]
    pub repo_root: Option<PathBuf>,

    /// Emit progress and diagnostic messages.
    #[arg(long, global = true)]
    pub verbose: bool,

    /// Management subcommands (index, update, status).
    #[command(subcommand)]
    pub command: Option<ManageCommand>,
}
