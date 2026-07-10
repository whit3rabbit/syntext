//! Count-matches renderer: prints exact per-file match counts.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::PathBuf;

use crate::path_util::path_bytes;
use crate::Config;

use super::color::write_styled;
use super::{compile_output_regex, ColorStyles};
use crate::cli::search::SearchArgs;

pub(in crate::cli) fn render_count_matches(
    _config: &Config,
    matches: &[crate::SearchMatch],
    args: &SearchArgs,
) -> io::Result<i32> {
    let styles = ColorStyles::default();
    let re = match compile_output_regex(args) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("st: invalid pattern: {e}");
            return Ok(2);
        }
    };

    // Count matches (rg counts submatches, so >1 per line is possible) from the
    // in-memory result set, not by re-reading files at render time. This keeps
    // the counts consistent with the search snapshot and honors -m/--max-count
    // truncation already applied to `matches` in run_search. BTreeMap preserves
    // the same sorted-by-path output order the previous BTreeSet gave.
    let mut counts: BTreeMap<&PathBuf, usize> = BTreeMap::new();
    for m in matches {
        let n = re.find_iter(&m.line_content).count();
        *counts.entry(&m.path).or_insert(0) += n;
    }

    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut found_any = false;
    for (path, count) in &counts {
        // A matched line can still yield zero submatches under an output regex
        // (e.g. -w/-x boundary wrapping); skip those so we never print `path:0`.
        if *count == 0 {
            continue;
        }
        found_any = true;
        if args.no_filename {
            writeln!(out, "{count}")?;
        } else {
            write_styled(&mut out, args.color, styles.path, path_bytes(path).as_ref())?;
            writeln!(out, ":{count}")?;
        }
    }

    Ok(if found_any { 0 } else { 1 })
}
