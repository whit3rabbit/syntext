//! CLI entry point: `rl <pattern>`, `rl index`, `rl status`, `rl update`.
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
/// Use `rl index` to build the index first, then `rl <pattern>` to search.
#[derive(Parser)]
#[command(name = "rl", version, about, disable_help_subcommand = true)]
pub struct Cli {
    /// Pattern to search (regex by default). Use -F for literal, -e to avoid
    /// subcommand name conflicts (e.g. `rl -e index`).
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

    /// Override index directory (default: .ripline/ at repo root).
    #[arg(long, global = true, env = "RIPLINE_INDEX_DIR")]
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
                    eprintln!("rl: a pattern is required (try `rl --help`)");
                    return 2;
                }
            };
            let ctx = cli.context.unwrap_or(0);
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

/// Resolve Config from CLI args and environment.
fn resolve_config(cli: &Cli) -> Config {
    let repo_root = cli
        .repo_root
        .clone()
        .or_else(detect_repo_root)
        .unwrap_or_else(|| PathBuf::from("."));

    let index_dir = cli
        .index_dir
        .clone()
        .unwrap_or_else(|| repo_root.join(".ripline"));

    let max_file_size = std::env::var("RIPLINE_MAX_FILE_SIZE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10 * 1024 * 1024);

    Config {
        max_file_size,
        max_segments: 10,
        index_dir,
        repo_root,
        verbose: false,
    }
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

#[cfg(test)]
mod tests {
    use std::fs;

    use clap::Parser;

    use crate::index::Index;
    use crate::{Config, SearchOptions};

    use super::{Cli, ManageCommand, manage::cmd_index, manage::cmd_update};

    #[test]
    fn search_works_without_subcommand() {
        let cli = Cli::try_parse_from(["rl", "fn_hello"]).expect("parse failed");
        assert!(cli.command.is_none());
        assert_eq!(cli.pattern.as_deref(), Some("fn_hello"));
    }

    #[test]
    fn fixed_strings_short_flag_is_capital_f() {
        let cli = Cli::try_parse_from(["rl", "-F", "fn.hello"]).expect("parse failed");
        assert!(cli.fixed_strings);
        assert_eq!(cli.pattern.as_deref(), Some("fn.hello"));
    }

    #[test]
    fn files_with_matches_short_flag_is_lowercase_l() {
        let cli = Cli::try_parse_from(["rl", "-l", "pattern"]).expect("parse failed");
        assert!(cli.files_with_matches);
    }

    #[test]
    fn context_flag_sets_both_before_and_after() {
        let cli = Cli::try_parse_from(["rl", "-C", "3", "pattern"]).expect("parse failed");
        assert_eq!(cli.context, Some(3));
    }

    #[test]
    fn manage_index_subcommand_still_routes_correctly() {
        let cli = Cli::try_parse_from(["rl", "index"]).expect("parse failed");
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
}
