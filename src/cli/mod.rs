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
    let config = resolve_config(&cli);

    match cli.command {
        Command::Index { force, stats, quiet } => cmd_index(config, force, stats, quiet),
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
                pattern, literal, ignore_case, file_type, max_count, count, json, quiet,
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

fn cmd_index(config: Config, force: bool, stats: bool, _quiet: bool) -> i32 {
    let index = if force || !config.index_dir.join("manifest.json").exists() {
        match Index::build(config) {
            Ok(idx) => idx,
            Err(e) => {
                eprintln!("ripline index: {e}");
                return 2;
            }
        }
    } else {
        match Index::open(config) {
            Ok(idx) => idx,
            Err(e) => {
                eprintln!("ripline index: {e}");
                return 2;
            }
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
        let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
        for m in &results {
            *counts.entry(m.path.to_string_lossy().into_owned()).or_default() += 1;
        }
        for (path, n) in &counts {
            println!("{path}:{n}");
        }
    } else if args.json {
        for m in &results {
            let path_str = m.path.to_string_lossy();
            let escaped_path = serde_json::to_string(path_str.as_ref()).unwrap_or_else(|_| "\"\"".to_string());
            let escaped_content = serde_json::to_string(&m.line_content).unwrap_or_else(|_| "\"\"".to_string());
            println!(
                "{{\"path\":{},\"line\":{},\"content\":{},\"byte_offset\":{}}}",
                escaped_path,
                m.line_number,
                escaped_content,
                m.byte_offset,
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

    // Detect changed files via git diff against HEAD.
    let diff_output = match std::process::Command::new("git")
        .arg("-C")
        .arg(&config.repo_root)
        .args(["diff", "--name-only", "HEAD"])
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            eprintln!("ripline update: failed to run git diff: {e}");
            return 2;
        }
    };

    let diff_stdout = String::from_utf8_lossy(&diff_output.stdout);
    let mut changed: Vec<String> = diff_stdout
        .lines()
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect();

    // Pick up new untracked files that git-diff doesn't report.
    if let Ok(ut_output) = std::process::Command::new("git")
        .arg("-C")
        .arg(&config.repo_root)
        .args(["ls-files", "--others", "--exclude-standard"])
        .output()
    {
        let ut_stdout = String::from_utf8_lossy(&ut_output.stdout);
        for line in ut_stdout.lines().filter(|l| !l.is_empty()) {
            if !changed.iter().any(|c| c == line) {
                changed.push(line.to_string());
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
