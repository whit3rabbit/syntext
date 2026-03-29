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
use search::{SearchArgs, cmd_search};

/// Fast code search with index acceleration. rg-compatible interface.
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

    /// Case-insensitive search.
    #[arg(short = 'i', long = "ignore-case")]
    pub ignore_case: bool,

    /// Only match whole words (wraps pattern in \b...\b).
    #[arg(short = 'w', long = "word-regexp")]
    pub word_regexp: bool,

    /// Invert matching: print lines that do NOT match.
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
    #[arg(short = 'l', long = "files-with-matches")]
    pub files_with_matches: bool,

    /// Print count of matching lines per file.
    #[arg(short = 'c', long = "count")]
    pub count: bool,

    /// Stop after NUM total matches.
    #[arg(short = 'm', long = "max-count", value_name = "NUM")]
    pub max_count: Option<usize>,

    /// Suppress output; exit 0 if any match found.
    #[arg(short = 'q', long = "quiet")]
    pub quiet: bool,

    /// Output as NDJSON (ripgrep-compatible format).
    #[arg(long = "json")]
    pub json: bool,

    /// Group matches under their file name (like rg default on a tty).
    #[arg(long = "heading", overrides_with = "no_heading")]
    pub heading: bool,

    /// Print path:line:content on each line (default; overrides --heading).
    #[arg(long = "no-heading", overrides_with = "heading")]
    pub no_heading: bool,

    /// Suppress line numbers in output.
    #[arg(short = 'N', long = "no-line-number")]
    pub no_line_number: bool,

    /// Suppress file names in output.
    #[arg(long = "no-filename")]
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
        Some(ManageCommand::Index { force, stats, quiet }) => {
            cmd_index(config, force, stats, quiet)
        }
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
mod tests {
    use std::fs;

    use clap::Parser;

    use crate::index::Index;
    use crate::{Config, SearchOptions};

    use super::{Cli, ManageCommand, manage::cmd_index, manage::cmd_update};

    #[test]
    fn search_works_without_subcommand() {
        let cli = Cli::try_parse_from(["st", "fn_hello"]).expect("parse failed");
        assert!(cli.command.is_none());
        assert_eq!(cli.pattern.as_deref(), Some("fn_hello"));
    }

    #[test]
    fn fixed_strings_short_flag_is_capital_f() {
        let cli = Cli::try_parse_from(["st", "-F", "fn.hello"]).expect("parse failed");
        assert!(cli.fixed_strings);
        assert_eq!(cli.pattern.as_deref(), Some("fn.hello"));
    }

    #[test]
    fn files_with_matches_short_flag_is_lowercase_l() {
        let cli = Cli::try_parse_from(["st", "-l", "pattern"]).expect("parse failed");
        assert!(cli.files_with_matches);
    }

    #[test]
    fn context_flag_sets_both_before_and_after() {
        let cli = Cli::try_parse_from(["st", "-C", "3", "pattern"]).expect("parse failed");
        assert_eq!(cli.context, Some(3));
    }

    #[test]
    fn manage_index_subcommand_still_routes_correctly() {
        let cli = Cli::try_parse_from(["st", "index"]).expect("parse failed");
        assert!(cli.pattern.is_none());
        assert!(matches!(cli.command, Some(ManageCommand::Index { .. })));
    }

    #[test]
    fn cmd_index_rebuilds_existing_index_without_force() {
        let repo = tempfile::TempDir::new().unwrap();
        let index_dir = tempfile::TempDir::new().unwrap();

        fs::create_dir_all(repo.path().join("src")).unwrap();
        let file = repo.path().join("src/main.rs");
        fs::write(&file, "fn first_version() {}\n").unwrap();

        let config = Config {
            index_dir: index_dir.path().to_path_buf(),
            repo_root: repo.path().to_path_buf(),
            ..Config::default()
        };

        assert_eq!(cmd_index(config.clone(), false, false, true), 0);

        fs::write(&file, "fn second_version() {}\n").unwrap();
        assert_eq!(cmd_index(config.clone(), false, false, true), 0);

        let index = Index::open(config).unwrap();
        let opts = SearchOptions::default();
        let first = index.search("first_version", &opts).unwrap();
        let second = index.search("second_version", &opts).unwrap();

        assert!(first.is_empty(), "old content should be gone after rebuild");
        assert_eq!(
            second.len(),
            1,
            "new content should be indexed after rebuild"
        );
    }

    #[test]
    fn render_with_context_emits_separator_between_blocks() {
        use std::fs;
        let dir = tempfile::TempDir::new().unwrap();

        // 20-line file; matches on lines 3 and 18 (far enough apart that context=2 creates two separate blocks).
        let content: String = (1..=20)
            .map(|i| {
                if i == 3 || i == 18 {
                    format!("target_token line {i}\n")
                } else {
                    format!("other line {i}\n")
                }
            })
            .collect();
        let path = dir.path().join("sample.rs");
        fs::write(&path, &content).unwrap();

        let matches = vec![
            crate::SearchMatch {
                path: std::path::PathBuf::from("sample.rs"),
                line_number: 3,
                line_content: b"target_token line 3".to_vec(),
                byte_offset: 0,
                submatch_start: 0,
                submatch_end: "target_token".len(),
            },
            crate::SearchMatch {
                path: std::path::PathBuf::from("sample.rs"),
                line_number: 18,
                line_content: b"target_token line 18".to_vec(),
                byte_offset: 0,
                submatch_start: 0,
                submatch_end: "target_token".len(),
            },
        ];

        let config = Config {
            repo_root: dir.path().to_path_buf(),
            ..Config::default()
        };

        let args = super::search::SearchArgs {
            pattern: "target_token".to_string(),
            after_context: 2,
            before_context: 2,
            ..super::search::SearchArgs::default()
        };

        let mut buf = Vec::<u8>::new();
        super::render::render_with_context_to(&config, &matches, &args, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();

        // Should contain a -- separator between the two non-contiguous context blocks.
        // Block 1: lines 1-5 (around match at line 3)
        // Block 2: lines 16-20 (around match at line 18)
        // Gap between: lines 6-15 are not printed.
        assert!(output.contains("--\n"), "Expected '--' separator between context blocks, got:\n{output}");

        // Match lines should use ':' separator.
        assert!(output.contains(":target_token line 3"), "Expected ':' for match line");
        assert!(output.contains(":target_token line 18"), "Expected ':' for match line");

        // Context lines should use '-' separator.
        assert!(output.contains("-other line 1") || output.contains("-other line 2"),
            "Expected '-' for context lines before first match");
    }

    #[test]
    fn json_output_is_ndjson_with_type_envelope() {
        let m = crate::SearchMatch {
            path: std::path::PathBuf::from("src/foo.rs"),
            line_number: 5,
            line_content: b"fn foo() {}".to_vec(),
            byte_offset: 3,
            submatch_start: 3,
            submatch_end: 6,
        };
        let line = super::render::format_match_json(&m);
        let parsed: serde_json::Value = serde_json::from_str(&line).expect("must be valid JSON");
        assert_eq!(parsed["type"], "match");
        assert_eq!(parsed["data"]["line_number"], 5);
        assert_eq!(parsed["data"]["lines"]["text"], "fn foo() {}\n");
        assert_eq!(parsed["data"]["path"]["text"], "src/foo.rs");
    }

    #[test]
    fn cmd_update_on_repo_with_no_commits() {
        let repo = tempfile::TempDir::new().unwrap();
        let index_dir = tempfile::TempDir::new().unwrap();

        // Initialize git repo with no commits.
        std::process::Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["init"])
            .output()
            .unwrap();

        // Create a file and build the index.
        fs::write(repo.path().join("hello.rs"), "fn hello() {}\n").unwrap();
        let config = Config {
            index_dir: index_dir.path().to_path_buf(),
            repo_root: repo.path().to_path_buf(),
            ..Config::default()
        };
        assert_eq!(cmd_index(config.clone(), false, false, true), 0);

        // cmd_update should not crash on a repo with no commits.
        // git diff HEAD fails, but we fall through to untracked file detection.
        let code = cmd_update(config, false, true);
        assert_ne!(
            code, 2,
            "cmd_update should not error on repo with no commits"
        );
    }

    #[test]
    fn max_file_size_is_clamped_to_1gb() {
        // Use clamp_max_file_size() directly rather than setting the env var.
        // std::env::set_var is not thread-safe: the test harness runs tests in
        // parallel, so a set_var / remove_var pair in one test can affect any
        // concurrent test that reads SYNTEXT_MAX_FILE_SIZE from the environment,
        // causing non-deterministic failures. Testing the inner function avoids
        // the global process state entirely.
        let result = super::clamp_max_file_size(Some(2_147_483_648)); // 2 GiB raw
        assert_eq!(
            result,
            super::MAX_FILE_SIZE_CEILING,
            "value above 1 GiB must be clamped to MAX_FILE_SIZE_CEILING"
        );
        // The +1 used in commit_batch must not overflow after clamping.
        assert!(
            result.checked_add(1).is_some(),
            "clamped value + 1 must not overflow"
        );
    }
}
