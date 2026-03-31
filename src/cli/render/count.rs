//! Count-matches renderer: prints exact per-file match counts.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::PathBuf;

use crate::path_util::path_bytes;
use crate::search::lines::for_each_line;
use crate::Config;

use crate::cli::search::SearchArgs;
use super::{compile_output_regex, read_repo_file_bytes};

pub(in crate::cli) fn render_count_matches(
    config: &Config,
    matches: &[crate::SearchMatch],
    args: &SearchArgs,
) -> io::Result<i32> {
    let re = match compile_output_regex(args) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("st: invalid pattern: {e}");
            return Ok(2);
        }
    };

    let mut per_file: BTreeMap<PathBuf, usize> = BTreeMap::new();
    for m in matches {
        per_file.entry(m.path.clone()).or_insert(0);
    }

    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut found_any = false;
    for path in per_file.keys() {
        let raw_bytes = match read_repo_file_bytes(config, path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let file_bytes = crate::index::normalize_encoding(&raw_bytes, config.verbose);

        let mut count = 0usize;
        for_each_line(file_bytes.as_ref(), |_, _, line| {
            count += re.find_iter(line).count();
        });
        if count == 0 {
            continue;
        }
        found_any = true;
        if args.no_filename {
            writeln!(out, "{count}")?;
        } else {
            out.write_all(path_bytes(path).as_ref())?;
            writeln!(out, ":{count}")?;
        }
    }

    Ok(if found_any { 0 } else { 1 })
}
