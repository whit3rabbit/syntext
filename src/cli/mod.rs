//! CLI entry point: `st <pattern>`, `st index`, `st status`, `st update`.
//!
//! Uses clap derive for argument parsing. Output format is grep-compatible
//! by default, with `--json` for machine-readable output.

mod bench;
mod manage;
mod render;
mod search;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::Config;

use bench::cmd_bench_search;
use manage::{cmd_index, cmd_status, cmd_update};
use search::{cmd_search, SearchArgs};

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
    #[arg(short = 's', long = "case-sensitive", overrides_with = "ignore_case")]
    pub case_sensitive: bool,

    /// Case-insensitive search.
    #[arg(short = 'i', long = "ignore-case", overrides_with = "case_sensitive")]
    pub ignore_case: bool,

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
    #[arg(
        short = 'e',
        long = "regexp",
        value_name = "PATTERN",
        conflicts_with = "pattern"
    )]
    pub regexp: Option<String>,

    // --- Output format ---
    /// Print only paths of files with at least one match.
    #[arg(
        short = 'l',
        long = "files-with-matches",
        overrides_with_all = ["count", "json"]
    )]
    pub files_with_matches: bool,

    /// Print count of matching lines per file.
    #[arg(short = 'c', long = "count", overrides_with_all = ["files_with_matches", "json"])]
    pub count: bool,

    /// Limit the number of matching lines printed per file.
    #[arg(short = 'm', long = "max-count", value_name = "NUM")]
    pub max_count: Option<usize>,

    /// Suppress output; exit 0 if any match found.
    #[arg(short = 'q', long = "quiet")]
    pub quiet: bool,

    /// Output as NDJSON (ripgrep-style format).
    #[arg(long = "json", overrides_with_all = ["files_with_matches", "count"])]
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

/// Management subcommands dispatched from the top-level CLI.
#[derive(Subcommand)]
pub enum ManageCommand {
    /// Build or rebuild the search index.
    Index {
        /// Rebuild from scratch even if an index exists.
        #[arg(long)]
        force: bool,
        /// Print statistics after build.
        #[arg(long)]
        stats: bool,
        /// Suppress progress output.
        #[arg(short, long)]
        quiet: bool,
    },
    /// Show index statistics.
    Status {
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Incrementally update the index for changed files.
    Update {
        /// Force flush overlay to segment.
        #[arg(long)]
        flush: bool,
        /// Suppress output.
        #[arg(short, long)]
        quiet: bool,
    },
    #[command(hide = true)]
    BenchSearch {
        #[arg(long = "query", required = true)]
        queries: Vec<String>,
        #[arg(long, default_value_t = 1)]
        iterations: usize,
        #[arg(long, default_value_t = 0)]
        warmups: usize,
    },
}

/// Run the CLI. Returns the process exit code.
pub fn run() -> i32 {
    let cli = Cli::parse();
    let mut config = resolve_config(&cli);
    config.verbose = cli.verbose;

    match cli.command {
        Some(ManageCommand::Index {
            force,
            stats,
            quiet,
        }) => cmd_index(config, force, stats, quiet),
        Some(ManageCommand::Status { json }) => cmd_status(config, json),
        Some(ManageCommand::Update { flush, quiet }) => cmd_update(config, flush, quiet),
        Some(ManageCommand::BenchSearch {
            queries,
            iterations,
            warmups,
        }) => cmd_bench_search(config, &queries, iterations, warmups),
        None => {
            let pattern = match cli.pattern.or(cli.regexp) {
                Some(p) => p,
                None => {
                    eprintln!("st: a pattern is required (try `st --help`)");
                    return 2;
                }
            };
            let ctx = cli.context.unwrap_or(0);
            // no_heading is the default behavior; this flag exists for rg compatibility.
            let search_args = SearchArgs {
                pattern,
                paths: cli.paths,
                fixed_strings: cli.fixed_strings,
                ignore_case: cli.ignore_case,
                word_regexp: cli.word_regexp,
                line_regexp: cli.line_regexp,
                invert_match: cli.invert_match,
                files_with_matches: cli.files_with_matches,
                count: cli.count,
                max_count: cli.max_count,
                quiet: cli.quiet,
                json: cli.json,
                heading: cli.heading,
                no_line_number: cli.no_line_number,
                no_filename: cli.no_filename,
                after_context: cli.after_context.unwrap_or(ctx),
                before_context: cli.before_context.unwrap_or(ctx),
                file_type: cli.file_type,
                type_not: cli.type_not,
                glob: cli.glob,
            };
            cmd_search(config, &search_args)
        }
    }
}

/// Hard ceiling for `SYNTEXT_MAX_FILE_SIZE` (1 GiB).
///
/// Prevents `take(0)` overflow when the env var is set to a very large value.
const MAX_FILE_SIZE_CEILING: u64 = 1_073_741_824;

/// Resolve Config from CLI args and environment.
fn resolve_config(cli: &Cli) -> Config {
    let repo_root = cli
        .repo_root
        .clone()
        .or_else(detect_repo_root)
        .unwrap_or_else(|| PathBuf::from("."));

    let index_dir = {
        let raw = cli
            .index_dir
            .clone()
            .unwrap_or_else(|| repo_root.join(".syntext"));
        // Security: reject paths that overlap system directories before any
        // fs::create_dir_all or fs::set_permissions call in build_index.
        if let Err(msg) = validate_index_dir(&raw) {
            eprintln!("{msg}");
            std::process::exit(2);
        }
        raw
    };

    let max_file_size = parse_max_file_size();

    Config {
        max_file_size,
        max_segments: 10,
        index_dir,
        repo_root,
        verbose: false,
        strict_permissions: true,
    }
}

/// Read and clamp `SYNTEXT_MAX_FILE_SIZE` from the environment.
///
/// Returns the configured value clamped to [`MAX_FILE_SIZE_CEILING`], or
/// the 10 MiB default when the variable is absent or unparseable.
fn parse_max_file_size() -> u64 {
    clamp_max_file_size(
        std::env::var("SYNTEXT_MAX_FILE_SIZE")
            .ok()
            .and_then(|v| v.parse::<u64>().ok()),
    )
}

/// Apply the 10 MiB default and the 1 GiB ceiling to an optional raw value.
///
/// Extracted from `parse_max_file_size` so tests can exercise the clamping
/// logic directly without mutating the process environment via `set_var`.
/// `std::env::set_var` is not thread-safe: Rust's test harness runs tests in
/// parallel, and any concurrent test reading `SYNTEXT_MAX_FILE_SIZE` during a
/// `set_var` / `remove_var` pair would observe the injected value, causing
/// non-deterministic test behaviour.
fn clamp_max_file_size(raw: Option<u64>) -> u64 {
    raw.unwrap_or(10 * 1024 * 1024).min(MAX_FILE_SIZE_CEILING)
}

/// Walk up from CWD looking for a `.git` directory.
fn detect_repo_root() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        if dir.join(".git").exists() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Reject `index_dir` values that overlap known-sensitive system path prefixes.
///
/// `--index-dir` / `SYNTEXT_INDEX_DIR` is passed directly to
/// `fs::create_dir_all` + `fs::set_permissions(0o700)` in `build_index`.
/// If the value is sourced from untrusted input (e.g., a CI artifact field)
/// and `st index` runs as root, a value like `/etc` would `chmod 0700 /etc`,
/// making it inaccessible to non-root processes and breaking the system.
///
/// Only absolute paths matching a known-sensitive prefix are rejected.
/// Relative paths and paths under user-owned directories are always accepted.
#[cfg(unix)]
fn validate_index_dir(index_dir: &std::path::Path) -> Result<(), String> {
    if !index_dir.is_absolute() {
        return Ok(());
    }
    const SENSITIVE_PREFIXES: &[&str] = &[
        "/etc",
        "/usr",
        "/bin",
        "/sbin",
        "/lib",
        "/lib64",
        "/sys",
        "/proc",
        "/dev",
        "/boot",
        "/root",
        // macOS: /etc and /var are symlinks into /private; check both.
        "/System",
        "/Library",
        "/private/etc",
        "/private/var/root",
    ];
    let dir_str = index_dir.to_string_lossy();
    for prefix in SENSITIVE_PREFIXES {
        // Reject exact match (e.g., /etc) or subpath (e.g., /etc/syntext).
        if dir_str == *prefix || dir_str.starts_with(&format!("{prefix}/")) {
            return Err(format!(
                "st: refusing --index-dir '{}': overlaps system path '{prefix}'; \
                 use a path under the repository or a user-owned directory",
                index_dir.display(),
            ));
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_index_dir(_index_dir: &std::path::Path) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests;
