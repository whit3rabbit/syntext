//! CLI entry point: `ripline index`, `ripline search`, `ripline status`, `ripline update`.
//!
//! Uses clap derive for argument parsing. Output format is grep-compatible
//! by default, with `--json` for machine-readable output.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::index::Index;
use crate::{Config, SearchOptions};

/// Hybrid code search index for agent workflows.
#[derive(Parser)]
#[command(name = "ripline", version, about)]
pub struct Cli {
    /// Override index directory (default: .ripline/ in repo root).
    #[arg(long, global = true, env = "RIPLINE_INDEX_DIR")]
    index_dir: Option<PathBuf>,

    /// Override repository root (default: auto-detect via .git).
    #[arg(long, global = true)]
    repo_root: Option<PathBuf>,

    /// Increase verbosity.
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Build or rebuild the search index.
    Index {
        /// Rebuild from scratch even if index exists.
        #[arg(long)]
        force: bool,
        /// Print statistics after build.
        #[arg(long)]
        stats: bool,
        /// Suppress progress output.
        #[arg(short, long)]
        quiet: bool,
    },
    /// Search for a pattern in the indexed repository.
    Search {
        /// Regex pattern to search for.
        pattern: String,
        /// Restrict search to these paths.
        #[arg(trailing_var_arg = true)]
        paths: Vec<String>,
        /// Treat pattern as a literal string.
        #[arg(short = 'l', long = "literal")]
        literal: bool,
        /// Case-insensitive search.
        #[arg(short = 'i', long = "ignore-case")]
        ignore_case: bool,
        /// Restrict to file type (e.g. rs, py, js).
        #[arg(short = 't', long = "type")]
        file_type: Option<String>,
        /// Exclude file type.
        #[arg(short = 'T', long = "type-not")]
        type_not: Option<String>,
        /// Maximum results to return.
        #[arg(short = 'm', long = "max-count")]
        max_count: Option<usize>,
        /// Show only match count per file.
        #[arg(short = 'c', long)]
        count: bool,
        /// Output as newline-delimited JSON.
        #[arg(long)]
        json: bool,
        /// Suppress output; exit 0 if match, 1 if not.
        #[arg(short = 'q', long)]
        quiet: bool,
    },
    /// Show index status and statistics.
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
}

/// Run the CLI. Returns the process exit code.
pub fn run() -> i32 {
    let cli = Cli::parse();
    let mut config = resolve_config(&cli);
    // CLI defaults to verbose unless --quiet is passed per-subcommand (overridden below).
    // Library consumers get verbose=false (Config::default()).
    config.verbose = cli.verbose;

    match cli.command {
        Command::Index {
            force,
            stats,
            quiet,
        } => cmd_index(config, force, stats, quiet),
        Command::Search {
            pattern,
            paths: _,
            literal,
            ignore_case,
            file_type,
            type_not: _,
            max_count,
            count,
            json,
            quiet,
        } => {
            let search_args = SearchArgs {
                pattern,
                literal,
                ignore_case,
                file_type,
                max_count,
                count,
                json,
                quiet,
            };
            cmd_search(config, &search_args)
        }
        Command::Status { json } => cmd_status(config, json),
        Command::Update { flush, quiet } => cmd_update(config, flush, quiet),
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

// ---------------------------------------------------------------------------
// Subcommand handlers
// ---------------------------------------------------------------------------

fn cmd_index(mut config: Config, force: bool, stats: bool, quiet: bool) -> i32 {
    let _ = force;
    // --quiet suppresses library progress output; default CLI behavior is verbose.
    if quiet {
        config.verbose = false;
    } else if !config.verbose {
        // Neither --verbose nor --quiet: default to verbose for CLI users.
        config.verbose = true;
    }
    let index = match Index::build(config) {
        Ok(idx) => idx,
        Err(e) => {
            eprintln!("ripline index: {e}");
            return 2;
        }
    };

    if stats {
        let s = index.stats();
        println!("Documents: {}", s.total_documents);
        println!("Segments:  {}", s.total_segments);
        println!("Grams:     {}", s.total_grams);
    }
    0
}

struct SearchArgs {
    pattern: String,
    literal: bool,
    ignore_case: bool,
    file_type: Option<String>,
    max_count: Option<usize>,
    count: bool,
    json: bool,
    quiet: bool,
}

fn cmd_search(config: Config, args: &SearchArgs) -> i32 {
    let index = match Index::open(config) {
        Ok(idx) => idx,
        Err(e) => {
            eprintln!("ripline search: {e}");
            return 2;
        }
    };

    let effective_pattern = if args.literal {
        regex::escape(&args.pattern)
    } else {
        args.pattern.clone()
    };

    let opts = SearchOptions {
        case_insensitive: args.ignore_case,
        file_type: args.file_type.clone(),
        max_results: args.max_count,
        ..SearchOptions::default()
    };

    let results = match index.search(&effective_pattern, &opts) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("ripline search: {e}");
            return 2;
        }
    };

    if results.is_empty() {
        return 1;
    }

    if args.quiet {
        return 0;
    }

    if args.count {
        // Count per file
        let mut counts: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::new();
        for m in &results {
            *counts
                .entry(m.path.to_string_lossy().into_owned())
                .or_default() += 1;
        }
        for (path, n) in &counts {
            println!("{path}:{n}");
        }
    } else if args.json {
        for m in &results {
            let path_str = m.path.to_string_lossy();
            let escaped_path =
                serde_json::to_string(path_str.as_ref()).unwrap_or_else(|_| "\"\"".to_string());
            let escaped_content =
                serde_json::to_string(&m.line_content).unwrap_or_else(|_| "\"\"".to_string());
            println!(
                "{{\"path\":{},\"line\":{},\"content\":{},\"byte_offset\":{}}}",
                escaped_path, m.line_number, escaped_content, m.byte_offset,
            );
        }
    } else {
        for m in &results {
            println!("{}:{}:{}", m.path.display(), m.line_number, m.line_content);
        }
    }

    0
}

fn cmd_status(config: Config, json: bool) -> i32 {
    let index = match Index::open(config.clone()) {
        Ok(idx) => idx,
        Err(e) => {
            eprintln!("ripline status: {e}");
            return 2;
        }
    };

    let s = index.stats();
    if json {
        println!(
            "{{\"documents\":{},\"segments\":{},\"grams\":{},\"index_dir\":\"{}\"}}",
            s.total_documents,
            s.total_segments,
            s.total_grams,
            config.index_dir.display(),
        );
    } else {
        println!("Index:     {}", config.index_dir.display());
        println!("Documents: {}", s.total_documents);
        println!("Segments:  {}", s.total_segments);
        println!("Grams:     {}", s.total_grams);
        if let Some(ref commit) = s.base_commit {
            println!("Commit:    {commit}");
        }
    }
    0
}

fn cmd_update(config: Config, _flush: bool, quiet: bool) -> i32 {
    let index = match Index::open(config.clone()) {
        Ok(idx) => idx,
        Err(e) => {
            eprintln!("ripline update: {e}");
            return 2;
        }
    };

    let mut changed: Vec<String> = Vec::new();

    // Detect changed files via git diff against HEAD.
    // This fails on repos with no commits, which is fine -- we fall through
    // to untracked file detection below.
    if let Ok(diff_output) = std::process::Command::new("git")
        .arg("-C")
        .arg(&config.repo_root)
        .args(["diff", "--name-only", "HEAD"])
        .output()
    {
        if diff_output.status.success() {
            let diff_stdout = String::from_utf8_lossy(&diff_output.stdout);
            changed.extend(
                diff_stdout
                    .lines()
                    .filter(|l| !l.is_empty())
                    .map(String::from),
            );
        }
    }

    // Pick up staged changes (covers initial commit scenario where HEAD
    // doesn't exist yet).
    if let Ok(staged_output) = std::process::Command::new("git")
        .arg("-C")
        .arg(&config.repo_root)
        .args(["diff", "--name-only", "--cached"])
        .output()
    {
        if staged_output.status.success() {
            let staged_stdout = String::from_utf8_lossy(&staged_output.stdout);
            for line in staged_stdout.lines().filter(|l| !l.is_empty()) {
                if !changed.iter().any(|c| c == line) {
                    changed.push(line.to_string());
                }
            }
        }
    }

    // Pick up new untracked files that git-diff doesn't report.
    if let Ok(ut_output) = std::process::Command::new("git")
        .arg("-C")
        .arg(&config.repo_root)
        .args(["ls-files", "--others", "--exclude-standard"])
        .output()
    {
        if ut_output.status.success() {
            let ut_stdout = String::from_utf8_lossy(&ut_output.stdout);
            for line in ut_stdout.lines().filter(|l| !l.is_empty()) {
                if !changed.iter().any(|c| c == line) {
                    changed.push(line.to_string());
                }
            }
        }
    }

    if changed.is_empty() {
        if !quiet {
            println!("ripline: no changes detected");
        }
        return 0;
    }

    let mut count = 0;
    for path in &changed {
        let abs = config.repo_root.join(path);
        if abs.exists() {
            if let Err(e) = index.notify_change(&abs) {
                eprintln!("ripline update: {path}: {e}");
            } else {
                count += 1;
            }
        } else {
            if let Err(e) = index.notify_delete(&abs) {
                eprintln!("ripline update: {path}: {e}");
            } else {
                count += 1;
            }
        }
    }

    if let Err(e) = index.commit_batch() {
        eprintln!("ripline update: commit failed: {e}");
        return 2;
    }

    if !quiet {
        println!("ripline: updated {} file(s)", count);
    }
    0
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::index::Index;
    use crate::{Config, SearchOptions};

    use super::{cmd_index, cmd_update};

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
