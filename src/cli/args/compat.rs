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

/// Warn once per invocation about accepted-but-unimplemented flags so a
/// caller (including an agent) is never misled into trusting a filter or
/// engine override that silently did nothing. Purely diagnostic: every one of
/// these flags is parsed and then ignored.
pub(crate) fn warn_unimplemented(cli: &super::Cli) {
        // --pcre2 is not supported; warn and continue with default engine.
        if cli.compat.pcre2 {
            eprintln!("st: --pcre2 is not supported; using default regex engine");
        }

        // Flags that filter the result set but are not yet implemented.
        // Warn so callers (including agents) know their filter was dropped.
        if let Some(ref glob) = cli.compat.iglob {
            eprintln!(
                "st: --iglob '{glob}' is not implemented; results may include excluded paths (use -g '!{glob}' for negation)"
            );
        }
        if cli.compat.multiline {
            eprintln!(
                "st: --multiline (-U) is not supported; patterns containing \\n will not match across lines"
            );
        }
        if let Some(ref mfs) = cli.compat.max_filesize {
            eprintln!(
                "st: --max-filesize '{mfs}' is not implemented; file-size filtering is skipped"
            );
        }
        if let Some(ref ig) = cli.compat.ignore_file {
            eprintln!(
                "st: --ignore-file '{}' is not implemented; ignore rules from that file are skipped",
                ig.display()
            );
        }
        if !cli.colors.is_empty() {
            eprintln!(
                "st: --colors is not implemented; default match/path/line colors are used"
            );
        }

        // Semantically-dangerous silent flags: these are accepted (parsed)
        // but have NO effect, and silence here would mislead an agent into
        // thinking it searched more than it did. Warn so the dropped
        // behavior is visible. (Truly cosmetic no-ops like --sort path,
        // which is truthful since results are already path-sorted, are
        // intentionally NOT warned.)
        if cli.compat.unrestricted > 0 {
            eprintln!(
                "st: -u/--unrestricted is not implemented; hidden/.gitignore/binary files are not searched"
            );
        }
        if cli.compat.binary || cli.compat.text {
            eprintln!(
                "st: --binary/-a/--text is not implemented; binary-file handling matches ripgrep's text-mode default"
            );
        }
        if cli.compat.search_zip {
            eprintln!(
                "st: -z/--search-zip is not implemented; compressed files are not transparently decompressed"
            );
        }
        // More result-affecting silent no-ops: these change which bytes rg
        // would search or how it splits lines, and accepting them silently
        // would under-report what was actually searched.
        if cli.compat.follow {
            eprintln!(
                "st: -L/--follow is not implemented; symlinked paths outside the walk are not searched"
            );
        }
        if cli.compat.one_file_system {
            eprintln!(
                "st: --one-file-system is not implemented; other mount points under the search paths are not excluded"
            );
        }
        if cli.compat.null_data {
            eprintln!(
                "st: --null-data is not implemented; lines always split on '\\n' only"
            );
        }
        // --sort/--sortr are no-ops (results are always path-sorted), so
        // `--sort path`/`--sort none` are truthful. Warn only for other
        // sort keys, which the user expects to actually reorder results.
        for (opt, val) in [
            ("--sort", cli.compat.sort.as_deref()),
            ("--sortr", cli.compat.sortr.as_deref()),
        ] {
            if let Some(v) = val {
                if v != "path" && v != "none" {
                    eprintln!(
                        "st: {opt} '{v}' is not implemented; results are always sorted by path"
                    );
                }
            }
        }
        // More search-affecting flags that are parsed but have no effect.
        // Warn so a caller (including an agent) is not misled into trusting
        // a preprocessor, alternate regex engine, encoding override, or
        // ad-hoc type definition that silently did nothing.
        if cli.compat.pre.is_some() || cli.compat.pre_glob.is_some() {
            eprintln!(
                "st: --pre/--pre-glob is not implemented; files are searched as-is, not through a preprocessor"
            );
        }
        if let Some(ref eng) = cli.compat.engine {
            eprintln!(
                "st: --engine '{eng}' is not implemented; the default regex engine is always used"
            );
        }
        if let Some(ref enc) = cli.compat.encoding {
            eprintln!(
                "st: --encoding '{enc}' is not implemented; encoding is auto-detected (UTF-8/UTF-16 BOM) only"
            );
        }
        if !cli.compat.type_add.is_empty() || !cli.compat.type_clear.is_empty() {
            eprintln!(
                "st: --type-add/--type-clear is not implemented; -t/-T use the built-in type definitions only"
            );
        }
}
