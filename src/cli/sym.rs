//! Symbol-lookup CLI helpers (`--sym`) and find-references (`--refs`).
//!
//! `--sym` results are a lookup, not a content match, so the grep-style output
//! modifiers do not apply; `--refs` produces content matches and supports the
//! full output set. This module centralizes the validation that
//! `run_and_render` runs before dispatching either.

use super::search::SearchArgs;

/// Reject flag combinations that are invalid for `--sym`/`--refs`. Returns
/// `Some(exit_code)` (with a message on stderr) on conflict, `None` otherwise.
/// The fields are always present (cfg-gated to `None` without the symbols
/// feature), so this compiles and is a no-op when symbols is off.
pub(super) fn reject_sym_refs_conflicts(args: &SearchArgs) -> Option<i32> {
    // --sym and --refs both resolve a name; both at once is ambiguous.
    if args.sym.is_some() && args.refs.is_some() {
        eprintln!("st: --sym and --refs are mutually exclusive");
        return Some(2);
    }
    // --sym-kind filters a name lookup; without --sym/--refs it has no target.
    if args.sym_kind.is_some() && args.sym.is_none() && args.refs.is_none() {
        eprintln!("st: --sym-kind requires --sym or --refs");
        return Some(2);
    }
    None
}

/// Reject output flags that make no sense for a symbol lookup. Returns
/// `Some(exit_code)` (with a message on stderr) for the first incompatible flag,
/// or `None` when the flags are compatible.
pub(super) fn reject_incompatible_symbol_flags(args: &SearchArgs) -> Option<i32> {
    let context = args.before_context > 0 || args.after_context > 0;
    // (is-set, "st: <flag> is not supported for symbol queries")
    let checks: [(bool, &str); 9] = [
        (args.count, "--count"),
        (args.count_matches, "--count-matches"),
        (args.only_matching, "-o/--only-matching"),
        (args.json, "--json"),
        (args.vimgrep, "--vimgrep"),
        (args.replace.is_some(), "-r/--replace"),
        (args.column, "--column"),
        (context, "context flags (-A, -B, -C)"),
        (args.invert_match, "-v/--invert-match"),
    ];
    for (is_set, flag) in checks {
        if is_set {
            eprintln!("st: {flag} is not supported for symbol queries");
            return Some(2);
        }
    }
    None
}
