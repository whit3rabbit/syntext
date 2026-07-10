use clap::Args;
use std::path::PathBuf;

/// No-op or compatibility options to mirror ripgrep interface.
#[derive(Args, Clone)]
pub struct CompatibilityArgs {
    // --- File discovery ---
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
}
