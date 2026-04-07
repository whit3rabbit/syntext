//! CLI entry point: `st <pattern>`, `st index`, `st status`, `st update`.
//!
//! Uses clap derive for argument parsing. Output format is grep-compatible
//! by default, with `--json` for machine-readable output.

pub mod args;
mod bench;
mod commands;
mod manage;
mod render;
mod scope;
mod search;

use std::path::PathBuf;

use clap::Parser;

use crate::Config;

pub use args::{Cli, ManageCommand};
use bench::cmd_bench_search;
use manage::{cmd_index, cmd_status, cmd_type_list, cmd_update};
use scope::cmd_files;
use search::{cmd_search, SearchArgs};

/// Run the CLI. Returns the process exit code.
pub fn run() -> i32 {
    let cli = Cli::parse();
    let mut config = resolve_config(&cli);
    config.verbose = cli.verbose || cli.debug;

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
            // --type-list and --files do not require a pattern.
            if cli.type_list {
                return cmd_type_list();
            }
            if cli.files {
                return cmd_files(config, &cli);
            }

            // When -e/--regexp supplies the pattern, clap still assigns the
            // first positional to `pattern` (it doesn't know it's a path).
            // Shift that positional into `paths` so `st -e "pat" dir` works
            // like ripgrep.  Multiple -e values are OR-combined with `|`.
            let (pattern, paths) = if !cli.regexp.is_empty() {
                let mut p = cli.paths;
                if let Some(pos) = cli.pattern {
                    p.insert(0, PathBuf::from(pos));
                }
                let combined = if cli.regexp.len() == 1 {
                    cli.regexp.into_iter().next().unwrap()
                } else {
                    cli.regexp
                        .iter()
                        .map(|r| format!("(?:{r})"))
                        .collect::<Vec<_>>()
                        .join("|")
                };
                (combined, p)
            } else {
                match cli.pattern {
                    Some(pat) => (pat, cli.paths),
                    None => {
                        eprintln!("st: a pattern is required (try `st --help`)");
                        return 2;
                    }
                }
            };

            // --pcre2 is not supported; warn and continue with default engine.
            if cli.pcre2 {
                eprintln!("st: --pcre2 is not supported; using default regex engine");
            }

            // --smart-case: case-insensitive if pattern has no uppercase chars.
            let ignore_case = if cli.smart_case && !cli.case_sensitive && !cli.ignore_case {
                !pattern.chars().any(|c| c.is_uppercase())
            } else {
                cli.ignore_case
            };

            // --pretty is an alias for --heading --line-number (color is no-op).
            let heading = cli.heading || cli.pretty;
            let line_number = cli.line_number || cli.pretty;

            let ctx = cli.context.unwrap_or(0);
            let search_args = SearchArgs {
                pattern,
                paths,
                fixed_strings: cli.fixed_strings,
                ignore_case,
                word_regexp: cli.word_regexp,
                line_regexp: cli.line_regexp,
                line_number,
                with_filename: cli.with_filename,
                invert_match: cli.invert_match,
                files_with_matches: cli.files_with_matches,
                files_without_match: cli.files_without_match,
                count: cli.count,
                count_matches: cli.count_matches,
                max_count: cli.max_count,
                quiet: cli.quiet,
                only_matching: cli.only_matching,
                json: cli.json,
                heading,
                no_line_number: cli.no_line_number,
                no_filename: cli.no_filename,
                after_context: cli.after_context.unwrap_or(ctx),
                before_context: cli.before_context.unwrap_or(ctx),
                file_type: cli.file_type,
                type_not: cli.type_not,
                glob: cli.glob,
                column: cli.column || cli.vimgrep,
                vimgrep: cli.vimgrep,
                replace: cli.replace,
                null: cli.null,
                context_separator: cli.context_separator,
                byte_offset: cli.byte_offset,
                trim: cli.trim,
                max_columns: cli.max_columns,
                search_stats: cli.search_stats,
                max_depth: cli.max_depth,
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
///
/// The core prefix-matching logic is in `overlaps_sensitive_prefix` so it can
/// be unit-tested on any platform.
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
    if let Some(matched) = overlaps_sensitive_prefix(&dir_str, SENSITIVE_PREFIXES, '/') {
        return Err(format!(
            "st: refusing --index-dir '{}': overlaps system path '{matched}'; \
             use a path under the repository or a user-owned directory",
            index_dir.display(),
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_index_dir(index_dir: &std::path::Path) -> Result<(), String> {
    if !index_dir.is_absolute() {
        return Ok(());
    }
    // Build sensitive prefixes from environment variables (handles non-standard
    // Windows installs, e.g. Windows on D:\). Fall back to hardcoded defaults.
    let mut sensitive: Vec<String> = Vec::new();
    for var in [
        "SYSTEMROOT",
        "PROGRAMFILES",
        "PROGRAMFILES(X86)",
        "PROGRAMDATA",
    ] {
        if let Some(val) = std::env::var_os(var) {
            let lower = val.to_string_lossy().to_lowercase().replace('/', "\\");
            if !sensitive.contains(&lower) {
                sensitive.push(lower);
            }
        }
    }
    for fb in [
        "c:\\windows",
        "c:\\program files",
        "c:\\program files (x86)",
        "c:\\programdata",
    ] {
        let s = fb.to_string();
        if !sensitive.contains(&s) {
            sensitive.push(s);
        }
    }
    // Normalize the input path: lowercase, forward slashes -> backslashes.
    let dir_lower = index_dir
        .to_string_lossy()
        .to_lowercase()
        .replace('/', "\\");
    let prefixes: Vec<&str> = sensitive.iter().map(|s| s.as_str()).collect();
    if let Some(matched) = overlaps_sensitive_prefix(&dir_lower, &prefixes, '\\') {
        return Err(format!(
            "st: refusing --index-dir '{}': overlaps system path '{matched}'; \
             use a path under the repository or a user-owned directory",
            index_dir.display(),
        ));
    }
    Ok(())
}

/// Return the matched prefix if `dir` equals or is a child of any entry in
/// `prefixes` (separated by `sep`). Platform-independent so it can be tested
/// on any OS.
fn overlaps_sensitive_prefix<'a>(dir: &str, prefixes: &[&'a str], sep: char) -> Option<&'a str> {
    for &prefix in prefixes {
        if dir == prefix || dir.starts_with(&format!("{prefix}{sep}")) {
            return Some(prefix);
        }
    }
    None
}

#[cfg(test)]
mod tests;
