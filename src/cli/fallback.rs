//! ripgrep/grep fallback for un-indexed searches.
//!
//! When `Index::open` reports a missing index, `st` shells out to `ripgrep`
//! (preferred) or `grep` (last resort) so the search still returns results.
//! This is the default; opt out with `SYNTEXT_FALLBACK_RG=0` (the `--fallback`
//! flag stays accepted and overrides the env var).
//!
//! Design notes:
//! - Triggered ONLY on a missing index. A corrupt index or lock conflict still
//!   fails loudly; we do not paper over real corruption.
//! - ripgrep receives the user's original argv (minus the few st-only tokens it
//!   cannot parse). `st`'s CLI is a deliberate superset of rg's, so the flags
//!   `st` treats as no-ops become real again, and rg's own `--json`/`--vimgrep`
//!   output is byte-identical to what `st` emits.
//! - grep cannot consume rg argv, so its command is reconstructed from the
//!   parsed `SearchArgs`. Output-format flags rg/`st` support but grep does not
//!   (`--json`, `--vimgrep`, `--heading`, `--column`, `-t/--type`) are dropped;
//!   this is the documented "reduced fidelity" of the grep path.
//! - The fallback child inherits stdio, so stdout streams byte-for-byte and the
//!   child's exit code is propagated. Informational notices go to stderr only.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::search::SearchArgs;
use crate::Config;

/// Decide and run the fallback path for a missing index. Returns the process
/// exit code to propagate.
pub(super) fn handle_missing_index(_config: &Config, args: &SearchArgs, index_dir: &Path) -> i32 {
    let dir = index_dir.display();

    // Only --sym/--refs genuinely require the symbol index (they produce
    // symbol-derived results). A bare --sym-kind carries no query of its own,
    // so it must not hijack a plain content search into an error; it is
    // stripped before the rg/grep fallback instead (see ST_VALUE_FLAGS).
    if args.sym.is_some() || args.refs.is_some() {
        eprintln!("st: symbol flags (--sym, --refs) are not supported without an index");
        return 2;
    }

    if !fallback_enabled(args) {
        // Fallback disabled via env: keep the actionable error, but say how to
        // get the fallback back.
        eprintln!("st: no index found at {dir}");
        eprintln!("st:   build one with `st index` (run inside the repo you want to search), or");
        eprintln!(
            "st:   unset SYNTEXT_FALLBACK_RG (or pass --fallback) to search with ripgrep/grep"
        );
        return 2;
    }

    // A `--quiet` search (or SYNTEXT_QUIET_FALLBACK) wants silence; suppress
    // the informational notice but still run the fallback tool (rg/grep honor
    // their own -q via argv). Without -q, SYNTEXT_QUIET_FALLBACK=1 gives the
    // same silence for standing env-var opt-ins, where a per-search notice
    // repeated on every invocation is pure stderr noise.
    let notice = !args.quiet && !quiet_fallback_requested();

    if let Some(rg) = resolve_rg_binary() {
        if notice {
            eprintln!(
                "st: no index at {dir}; using ripgrep fallback (build with `st index` for full speed)"
            );
        } else {
            log::debug!("st: silently using ripgrep fallback for missing index at {dir}");
        }
        return exec(&rg, filter_st_args(std::env::args_os().collect()));
    }

    if let Some(grep) = resolve_grep_binary() {
        if notice {
            eprintln!(
                "st: no index at {dir}; ripgrep (rg) not in PATH, using grep fallback (reduced fidelity)"
            );
        } else {
            log::debug!("st: silently using grep fallback for missing index at {dir}");
        }
        return exec(&grep, build_grep_args(args));
    }

    eprintln!(
        "st: no index at {dir}, and neither ripgrep (rg) nor grep is in PATH; run `st index` to build one"
    );
    2
}

/// True unless the user opted out via `SYNTEXT_FALLBACK_RG=0/false/no/off`.
/// The `--fallback` flag forces fallback on even when the env var disables it.
fn fallback_enabled(args: &SearchArgs) -> bool {
    if args.fallback {
        return true;
    }
    match std::env::var("SYNTEXT_FALLBACK_RG") {
        Ok(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
        Err(_) => true,
    }
}

/// True when the user asked for fallback notices to be silenced
/// (SYNTEXT_QUIET_FALLBACK=1/true/yes/on), separate from `-q`.
fn quiet_fallback_requested() -> bool {
    match std::env::var("SYNTEXT_QUIET_FALLBACK") {
        Ok(v) => matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => false,
    }
}

#[cfg(unix)]
fn resolve_rg_binary() -> Option<PathBuf> {
    crate::git_util::find_in_path("rg")
}

#[cfg(not(unix))]
fn resolve_rg_binary() -> Option<PathBuf> {
    crate::git_util::find_in_path("rg.exe")
}

#[cfg(unix)]
fn resolve_grep_binary() -> Option<PathBuf> {
    crate::git_util::find_in_path("grep")
}

#[cfg(not(unix))]
fn resolve_grep_binary() -> Option<PathBuf> {
    crate::git_util::find_in_path("grep.exe")
}

/// Spawn `bin` with `args`, inheriting stdio, and return its exit code.
/// No shell is involved; args are passed as an argv vector (no injection).
fn exec(bin: &Path, args: Vec<OsString>) -> i32 {
    match Command::new(bin).args(&args).status() {
        Ok(status) => status.code().unwrap_or(2),
        Err(e) => {
            eprintln!("st: failed to run fallback `{}`: {e}", bin.display());
            2
        }
    }
}

/// st-only flags that take a value (rg does not understand any of them).
const ST_VALUE_FLAGS: &[&str] = &["--repo-root", "--index-dir", "--index", "--sym-kind"];
/// st-only boolean flags (rg does not understand them).
const ST_BOOL_FLAGS: &[&str] = &["--verbose", "--fallback", "--no-update"];

/// Strip st-specific tokens from argv so the remainder is valid ripgrep input,
/// translating the st-only flags that carry search semantics into rg's own
/// spelling:
/// - `--rust`/`--rs` (st's extension filter "rs") becomes `-t rust`; rg's type
///   table has no "rs" and rejects `-t rs` outright.
/// - `--exclude-dir D` becomes `-g '!D/**' -g '!**/D/**'` (the same glob pair
///   st's own scope filter derives from the flag; rg has no --exclude-dir).
///
/// Flags with no rg equivalent (`--verbose`/`--fallback`/`--no-update`, and
/// `--repo-root`/`--index-dir`/`--index`/`--sym-kind` plus their value in
/// separate-token or `--flag=value` form) are dropped. argv[0] (the program
/// name) is dropped; everything else passes through untouched.
///
/// Known limitation: a value-form flag name appearing as the *value* of another
/// option (e.g. `st -e --index-dir`, searching for the literal "--index-dir")
/// would be mis-stripped. Fully avoiding this requires re-implementing clap's
/// parser; the case is vanishingly rare for the un-indexed-search use case.
fn filter_st_args(argv: Vec<OsString>) -> Vec<OsString> {
    let mut out = Vec::with_capacity(argv.len());
    let mut iter = argv.into_iter();
    let _ = iter.next(); // skip argv[0] (program name)
    let mut skip_value = false;
    let mut pending_exclude_dir = false;
    for arg in iter {
        if skip_value {
            skip_value = false;
            continue;
        }
        if pending_exclude_dir {
            pending_exclude_dir = false;
            push_exclude_dir_globs(&mut out, &arg.to_string_lossy());
            continue;
        }
        let s = arg.to_string_lossy();
        if ST_BOOL_FLAGS.contains(&s.as_ref()) {
            continue;
        }
        if s == "--rust" || s == "--rs" {
            out.push(OsString::from("-t"));
            out.push(OsString::from("rust"));
            continue;
        }
        if s == "--exclude-dir" {
            pending_exclude_dir = true;
            continue;
        }
        if let Some(dir) = s.strip_prefix("--exclude-dir=") {
            push_exclude_dir_globs(&mut out, dir);
            continue;
        }
        if ST_VALUE_FLAGS.contains(&s.as_ref()) {
            skip_value = true; // drop this flag and its separate value token
            continue;
        }
        if let Some(eq) = s.find('=') {
            if ST_VALUE_FLAGS.contains(&&s[..eq]) {
                continue; // `--flag=value` form
            }
        }
        out.push(arg);
    }
    out
}

/// Emit rg argv excluding `dir` the way st's own scope filter does (see
/// `exclude_dir_glob_pair` in `args/globs.rs`); rg has no native --exclude-dir.
fn push_exclude_dir_globs(out: &mut Vec<OsString>, dir: &str) {
    for glob in super::args::globs::exclude_dir_glob_pair(dir) {
        out.push(OsString::from("-g"));
        out.push(OsString::from(glob));
    }
}

/// Reconstruct a best-effort grep command from parsed search args.
///
/// Maps common match/output flags. Drops what grep cannot do (`--json`,
/// `--vimgrep`, `--heading`, `--column`, `--byte-offset`, `-t/--type`,
/// `--replace`, `--trim`, `--max-columns`). Defaults the regex engine to `-E`
/// (closer to rg) unless `-F` was requested. Glob filters map to grep's
/// `--include`/`--exclude` (basename-only matching, hence reduced fidelity).
fn build_grep_args(args: &SearchArgs) -> Vec<OsString> {
    let mut v: Vec<OsString> = Vec::new();
    let flag = |v: &mut Vec<OsString>, s: &str| v.push(OsString::from(s));

    flag(&mut v, "-r");
    if !args.no_line_number {
        flag(&mut v, "-n");
    }
    if args.fixed_strings {
        flag(&mut v, "-F");
    } else {
        flag(&mut v, "-E");
    }
    if args.ignore_case {
        flag(&mut v, "-i");
    }
    if args.word_regexp {
        flag(&mut v, "-w");
    }
    if args.line_regexp {
        flag(&mut v, "-x");
    }
    if args.invert_match {
        flag(&mut v, "-v");
    }
    if args.files_with_matches {
        flag(&mut v, "-l");
    }
    if args.files_without_match {
        flag(&mut v, "-L");
    }
    if args.count {
        flag(&mut v, "-c");
    }
    if args.only_matching {
        flag(&mut v, "-o");
    }
    if let Some(m) = args.max_count {
        v.push(OsString::from("-m"));
        v.push(OsString::from(m.to_string()));
    }
    if args.after_context > 0 {
        v.push(OsString::from("-A"));
        v.push(OsString::from(args.after_context.to_string()));
    }
    if args.before_context > 0 {
        v.push(OsString::from("-B"));
        v.push(OsString::from(args.before_context.to_string()));
    }
    if args.no_filename {
        flag(&mut v, "-h");
    } else if args.with_filename {
        flag(&mut v, "-H");
    }
    for g in &args.globs {
        if let Some(stripped) = g.strip_prefix('!') {
            v.push(OsString::from(format!("--exclude={stripped}")));
        } else {
            v.push(OsString::from(format!("--include={g}")));
        }
    }
    // grep matches --include/--exclude against basenames only, so the globs
    // st derives from --exclude-dir (`!D/**`) can never match there. grep has
    // a native --exclude-dir; use it for the faithful mapping.
    for d in &args.exclude_dirs {
        v.push(OsString::from(format!("--exclude-dir={d}")));
    }
    // `-e PATTERN` so patterns beginning with `-` are not mistaken for flags.
    v.push(OsString::from("-e"));
    v.push(OsString::from(&args.pattern));
    if args.paths.is_empty() {
        v.push(OsString::from("."));
    } else {
        for p in &args.paths {
            v.push(p.clone().into_os_string());
        }
    }
    v
}

#[cfg(test)]
#[path = "fallback_tests.rs"]
mod tests;
