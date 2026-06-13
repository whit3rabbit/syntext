//! ripgrep/grep fallback for un-indexed searches.
//!
//! When `Index::open` reports a missing index and the user has opted in
//! (`--fallback` or `SYNTEXT_FALLBACK_RG=1`), `st` shells out to `ripgrep`
//! (preferred) or `grep` (last resort) so the search still returns results.
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

    if !fallback_enabled(args) {
        // Opt-in is off: keep the actionable error, but advertise both remedies.
        eprintln!("st: no index found at {dir}");
        eprintln!("st:   build one with `st index` (run inside the repo you want to search), or");
        eprintln!("st:   set SYNTEXT_FALLBACK_RG=1 (or pass --fallback) to search with ripgrep/grep");
        return 2;
    }

    // A `--quiet` search wants silence; suppress the informational notice but
    // still run the fallback tool (rg/grep honor their own -q via argv).
    let notice = !args.quiet;

    if let Some(rg) = resolve_rg_binary() {
        if notice {
            eprintln!(
                "st: no index at {dir}; using ripgrep fallback (build with `st index` for full speed)"
            );
        }
        return exec(&rg, filter_st_args(std::env::args_os().collect()));
    }

    if let Some(grep) = resolve_grep_binary() {
        if notice {
            eprintln!(
                "st: no index at {dir}; ripgrep (rg) not in PATH, using grep fallback (reduced fidelity)"
            );
        }
        return exec(&grep, build_grep_args(args));
    }

    eprintln!(
        "st: no index at {dir}, and neither ripgrep (rg) nor grep is in PATH; run `st index` to build one"
    );
    2
}

/// True when the user opted into fallback via the flag or the env var.
fn fallback_enabled(args: &SearchArgs) -> bool {
    if args.fallback {
        return true;
    }
    match std::env::var("SYNTEXT_FALLBACK_RG") {
        Ok(v) => matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"),
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
const ST_VALUE_FLAGS: &[&str] = &["--repo-root", "--index-dir", "--index"];
/// st-only boolean flags (rg does not understand them).
const ST_BOOL_FLAGS: &[&str] = &["--verbose", "--fallback"];

/// Strip st-specific tokens from argv so the remainder is valid ripgrep input.
///
/// Drops `--verbose`/`--fallback` (no value) and `--repo-root`/`--index-dir`/
/// `--index` plus their value (separate-token or `--flag=value` form). argv[0]
/// (the program name) is dropped; everything else passes through untouched.
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
    for arg in iter {
        if skip_value {
            skip_value = false;
            continue;
        }
        let s = arg.to_string_lossy();
        if ST_BOOL_FLAGS.contains(&s.as_ref()) {
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
mod tests {
    use super::*;

    fn osv(items: &[&str]) -> Vec<OsString> {
        items.iter().map(OsString::from).collect()
    }

    fn to_strings(items: Vec<OsString>) -> Vec<String> {
        items
            .into_iter()
            .map(|o| o.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn filter_strips_bool_flags() {
        let got = filter_st_args(osv(&["st", "--verbose", "foo", "--fallback", "src"]));
        assert_eq!(to_strings(got), vec!["foo", "src"]);
    }

    #[test]
    fn filter_strips_value_flags_separate_form() {
        let got = filter_st_args(osv(&[
            "st",
            "--repo-root",
            "/tmp/r",
            "foo",
            "--index-dir",
            "/tmp/i",
            "src",
        ]));
        assert_eq!(to_strings(got), vec!["foo", "src"]);
    }

    #[test]
    fn filter_strips_value_flags_eq_form() {
        let got = filter_st_args(osv(&[
            "st",
            "--repo-root=/tmp/r",
            "--index=/tmp/i",
            "foo",
        ]));
        assert_eq!(to_strings(got), vec!["foo"]);
    }

    #[test]
    fn filter_preserves_rg_shared_flags() {
        let got = filter_st_args(osv(&[
            "st", "-i", "--json", "-A", "2", "-e", "foo", "src",
        ]));
        assert_eq!(
            to_strings(got),
            vec!["-i", "--json", "-A", "2", "-e", "foo", "src"]
        );
    }

    #[test]
    fn grep_args_map_common_flags() {
        let args = SearchArgs {
            pattern: "needle".to_string(),
            paths: vec![PathBuf::from("src")],
            ignore_case: true,
            word_regexp: true,
            after_context: 2,
            ..SearchArgs::default()
        };
        let got = to_strings(build_grep_args(&args));
        assert!(got.contains(&"-r".to_string()));
        assert!(got.contains(&"-n".to_string()));
        assert!(got.contains(&"-E".to_string()));
        assert!(got.contains(&"-i".to_string()));
        assert!(got.contains(&"-w".to_string()));
        assert_eq!(
            got.windows(2).find(|w| w[0] == "-A"),
            Some(["-A".to_string(), "2".to_string()].as_slice())
        );
        // pattern is passed via -e, paths trail.
        assert_eq!(
            got.windows(2).find(|w| w[0] == "-e"),
            Some(["-e".to_string(), "needle".to_string()].as_slice())
        );
        assert_eq!(got.last().unwrap(), "src");
    }

    #[test]
    fn grep_args_default_paths_to_dot_and_fixed_strings() {
        let args = SearchArgs {
            pattern: "lit".to_string(),
            fixed_strings: true,
            ..SearchArgs::default()
        };
        let got = to_strings(build_grep_args(&args));
        assert!(got.contains(&"-F".to_string()));
        assert!(!got.contains(&"-E".to_string()));
        assert_eq!(got.last().unwrap(), ".");
    }

    #[test]
    fn grep_args_map_globs_to_include_exclude() {
        let args = SearchArgs {
            pattern: "x".to_string(),
            globs: vec!["*.rs".to_string(), "!*.lock".to_string()],
            ..SearchArgs::default()
        };
        let got = to_strings(build_grep_args(&args));
        assert!(got.contains(&"--include=*.rs".to_string()));
        assert!(got.contains(&"--exclude=*.lock".to_string()));
    }
}
