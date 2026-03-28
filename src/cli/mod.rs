//! CLI entry point: `rl <pattern>`, `rl index`, `rl status`, `rl update`.
//!
//! Uses clap derive for argument parsing. Output format is grep-compatible
//! by default, with `--json` for machine-readable output.

use std::path::PathBuf;
use std::time::Instant;

use clap::{Parser, Subcommand};

use crate::index::Index;
use crate::{Config, SearchOptions};

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

struct SearchArgs {
    pattern: String,
    paths: Vec<PathBuf>,
    fixed_strings: bool,
    ignore_case: bool,
    word_regexp: bool,
    invert_match: bool,
    files_with_matches: bool,
    count: bool,
    max_count: Option<usize>,
    quiet: bool,
    json: bool,
    heading: bool,
    no_line_number: bool,
    no_filename: bool,
    after_context: usize,
    before_context: usize,
    file_type: Option<String>,
    type_not: Option<String>,
    glob: Option<String>,
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
            eprintln!("rl index: {e}");
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

fn cmd_search(config: Config, args: &SearchArgs) -> i32 {
    let index = match Index::open(config.clone()) {
        Ok(idx) => idx,
        Err(e) => {
            eprintln!("rl: {e}");
            return 2;
        }
    };

    let results = match run_search(&index, args) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("rl: {e}");
            return 2;
        }
    };

    if args.invert_match {
        return render_invert_match(&config, &results, args);
    }

    if results.is_empty() {
        return 1;
    }

    if args.quiet {
        return 0;
    }

    if args.files_with_matches {
        let mut seen = std::collections::BTreeSet::new();
        for m in &results {
            seen.insert(m.path.to_string_lossy().into_owned());
        }
        for path in &seen {
            println!("{path}");
        }
        return 0;
    }

    if args.count {
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
        return 0;
    }

    let has_context = args.after_context > 0 || args.before_context > 0;

    if args.json {
        render_json(&config, &results);
    } else if has_context {
        render_with_context(&config, &results, args);
    } else if args.heading {
        render_heading(&results, args);
    } else {
        render_flat(&results, args);
    }

    0
}

fn build_effective_pattern(args: &SearchArgs) -> String {
    let pat = if args.fixed_strings {
        regex::escape(&args.pattern)
    } else {
        args.pattern.clone()
    };
    if args.word_regexp {
        format!(r"\b{pat}\b")
    } else {
        pat
    }
}

fn run_search(
    index: &Index,
    args: &SearchArgs,
) -> Result<Vec<crate::SearchMatch>, crate::IndexError> {
    let effective_pattern = build_effective_pattern(args);

    let path_filter = args.glob.clone().or_else(|| paths_to_glob(&args.paths));

    let opts = SearchOptions {
        case_insensitive: args.ignore_case,
        file_type: args.file_type.clone(),
        exclude_type: args.type_not.clone(),
        max_results: args.max_count,
        path_filter,
    };

    index.search(&effective_pattern, &opts)
}

fn paths_to_glob(paths: &[PathBuf]) -> Option<String> {
    let first = paths.first()?;
    let s = first.to_string_lossy();
    if first.is_dir() {
        Some(format!("{s}/**"))
    } else {
        Some(s.into_owned())
    }
}

fn render_flat(matches: &[crate::SearchMatch], args: &SearchArgs) {
    for m in matches {
        let path = m.path.display();
        if args.no_filename && args.no_line_number {
            println!("{}", m.line_content);
        } else if args.no_filename {
            println!("{}:{}", m.line_number, m.line_content);
        } else if args.no_line_number {
            println!("{path}:{}", m.line_content);
        } else {
            println!("{path}:{}:{}", m.line_number, m.line_content);
        }
    }
}

fn render_heading(matches: &[crate::SearchMatch], args: &SearchArgs) {
    let mut current_path: Option<String> = None;
    for m in matches {
        let path_str = m.path.to_string_lossy().into_owned();
        if current_path.as_deref() != Some(&path_str) {
            if current_path.is_some() {
                println!();
            }
            println!("{path_str}");
            current_path = Some(path_str);
        }
        if args.no_line_number {
            println!("{}", m.line_content);
        } else {
            println!("{}:{}", m.line_number, m.line_content);
        }
    }
}

fn render_invert_match(
    config: &Config,
    candidate_matches: &[crate::SearchMatch],
    args: &SearchArgs,
) -> i32 {
    use std::collections::BTreeSet;
    use std::io::BufRead;

    let pattern = build_effective_pattern(args);
    let re = match regex::RegexBuilder::new(&pattern)
        .case_insensitive(args.ignore_case)
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("rl: invalid pattern: {e}");
            return 2;
        }
    };

    let files: BTreeSet<_> = candidate_matches
        .iter()
        .map(|m| config.repo_root.join(&m.path))
        .collect();

    let mut found_any = false;
    for abs_path in &files {
        let rel_path = abs_path
            .strip_prefix(&config.repo_root)
            .unwrap_or(abs_path);

        let file = match std::fs::File::open(abs_path) {
            Ok(f) => f,
            Err(_) => continue,
        };

        for (idx, line) in std::io::BufReader::new(file).lines().enumerate() {
            let line = match line {
                Ok(l) => l,
                Err(_) => break,
            };
            if !re.is_match(&line) {
                found_any = true;
                if !args.quiet {
                    let line_num = idx + 1;
                    if args.no_filename && args.no_line_number {
                        println!("{line}");
                    } else if args.no_filename {
                        println!("{line_num}:{line}");
                    } else if args.no_line_number {
                        println!("{}:{line}", rel_path.display());
                    } else {
                        println!("{}:{line_num}:{line}", rel_path.display());
                    }
                }
            }
        }
    }

    if found_any { 0 } else { 1 }
}

// Stub — will be filled in Task 4
fn render_with_context(
    _config: &Config,
    _matches: &[crate::SearchMatch],
    _args: &SearchArgs,
) {
    // TODO: implement in Task 4
}

// Stub — will be filled in Task 5
fn render_json(_config: &Config, _matches: &[crate::SearchMatch]) {
    // TODO: implement in Task 5
}

fn cmd_status(config: Config, json: bool) -> i32 {
    let index = match Index::open(config.clone()) {
        Ok(idx) => idx,
        Err(e) => {
            eprintln!("rl status: {e}");
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

#[derive(Debug, Clone)]
struct BenchQuerySpec {
    mode: BenchQueryMode,
    pattern: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BenchQueryMode {
    Literal,
    Regex,
}

fn parse_bench_query(value: &str) -> Result<BenchQuerySpec, String> {
    let (mode, pattern) = value.split_once(':').ok_or_else(|| {
        format!("invalid query {value:?}, expected literal:<pattern> or regex:<pattern>")
    })?;
    if pattern.is_empty() {
        return Err("query pattern must not be empty".to_string());
    }

    let mode = match mode {
        "literal" => BenchQueryMode::Literal,
        "regex" => BenchQueryMode::Regex,
        other => {
            return Err(format!(
                "invalid query mode {other:?}, expected literal or regex"
            ))
        }
    };

    Ok(BenchQuerySpec {
        mode,
        pattern: pattern.to_string(),
    })
}

fn summarize_samples(samples_ms: &[f64]) -> (f64, f64, f64) {
    let mut ordered = samples_ms.to_vec();
    ordered.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = ordered.len() / 2;
    let median = if ordered.len().is_multiple_of(2) {
        (ordered[mid - 1] + ordered[mid]) / 2.0
    } else {
        ordered[mid]
    };
    (median, ordered[0], ordered[ordered.len() - 1])
}

fn cmd_bench_search(config: Config, queries: &[String], iterations: usize, warmups: usize) -> i32 {
    if iterations == 0 {
        eprintln!("rl bench-search: iterations must be >= 1");
        return 2;
    }

    let parsed_queries: Result<Vec<_>, _> = queries.iter().map(|q| parse_bench_query(q)).collect();
    let parsed_queries = match parsed_queries {
        Ok(qs) => qs,
        Err(e) => {
            eprintln!("rl bench-search: {e}");
            return 2;
        }
    };

    let index = match Index::open(config) {
        Ok(idx) => idx,
        Err(e) => {
            eprintln!("rl bench-search: {e}");
            return 2;
        }
    };

    let mut results = Vec::with_capacity(parsed_queries.len());
    for query in &parsed_queries {
        let args = SearchArgs {
            pattern: query.pattern.clone(),
            paths: Vec::new(),
            fixed_strings: query.mode == BenchQueryMode::Literal,
            ignore_case: false,
            word_regexp: false,
            invert_match: false,
            files_with_matches: false,
            count: false,
            max_count: None,
            quiet: false,
            json: false,
            heading: false,
            no_line_number: false,
            no_filename: false,
            after_context: 0,
            before_context: 0,
            file_type: None,
            type_not: None,
            glob: None,
        };

        let count = match run_search(&index, &args) {
            Ok(r) => r.len(),
            Err(e) => {
                eprintln!("rl bench-search: {e}");
                return 2;
            }
        };

        for _ in 0..warmups {
            if let Err(e) = run_search(&index, &args) {
                eprintln!("rl bench-search: {e}");
                return 2;
            }
        }

        let mut samples = Vec::with_capacity(iterations);
        for _ in 0..iterations {
            let start = Instant::now();
            if let Err(e) = run_search(&index, &args) {
                eprintln!("rl bench-search: {e}");
                return 2;
            }
            samples.push(start.elapsed().as_secs_f64() * 1000.0);
        }

        let (median, min, max) = summarize_samples(&samples);
        let mode = match query.mode {
            BenchQueryMode::Literal => "literal",
            BenchQueryMode::Regex => "regex",
        };
        results.push(serde_json::json!({
            "query": format!("{mode}:{}", query.pattern),
            "count": count,
            "timings_ms": {
                "median_ms": (median * 1000.0).round() / 1000.0,
                "min_ms": (min * 1000.0).round() / 1000.0,
                "max_ms": (max * 1000.0).round() / 1000.0,
            }
        }));
    }

    println!(
        "{}",
        serde_json::json!({
            "queries": results,
        })
    );
    0
}

fn cmd_update(config: Config, _flush: bool, quiet: bool) -> i32 {
    let index = match Index::open(config.clone()) {
        Ok(idx) => idx,
        Err(e) => {
            eprintln!("rl update: {e}");
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
            println!("rl: no changes detected");
        }
        return 0;
    }

    let mut count = 0;
    let mut notify_errors = 0usize;
    for path in &changed {
        let abs = config.repo_root.join(path);
        if abs.exists() {
            if let Err(e) = index.notify_change(&abs) {
                eprintln!("rl update: {path}: {e}");
                notify_errors += 1;
            } else {
                count += 1;
            }
        } else {
            if let Err(e) = index.notify_delete(&abs) {
                eprintln!("rl update: {path}: {e}");
                notify_errors += 1;
            } else {
                count += 1;
            }
        }
    }

    if let Err(e) = index.commit_batch() {
        eprintln!("rl update: commit failed: {e}");
        return 2;
    }

    if !quiet {
        println!("rl: updated {} file(s)", count);
    }
    if notify_errors > 0 { 1 } else { 0 }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use clap::Parser;

    use crate::index::Index;
    use crate::{Config, SearchOptions};

    use super::{Cli, ManageCommand, cmd_index, cmd_update};

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
