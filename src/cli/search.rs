//! Search argument parsing, query execution, and result rendering.

use std::io::{self, Write};
use std::path::PathBuf;

use crate::index::Index;
use crate::path_util::path_bytes;
use crate::{Config, SearchOptions};

#[derive(Default)]
pub(super) struct SearchArgs {
    pub pattern: String,
    pub paths: Vec<PathBuf>,
    pub fixed_strings: bool,
    pub ignore_case: bool,
    pub word_regexp: bool,
    pub invert_match: bool,
    pub files_with_matches: bool,
    pub count: bool,
    pub max_count: Option<usize>,
    pub quiet: bool,
    pub json: bool,
    pub heading: bool,
    pub no_line_number: bool,
    pub no_filename: bool,
    pub after_context: usize,
    pub before_context: usize,
    pub file_type: Option<String>,
    pub type_not: Option<String>,
    pub glob: Option<String>,
}

pub(super) fn cmd_search(config: Config, args: &SearchArgs) -> i32 {
    if args.paths.len() > 1 {
        eprintln!(
            "st: warning: multiple path arguments not yet supported; using only {:?}",
            args.paths[0]
        );
    }
    let index = match Index::open(config.clone()) {
        Ok(idx) => idx,
        Err(e) => {
            eprintln!("st: {e}");
            return 2;
        }
    };

    let results = match run_search(&index, args) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("st: {e}");
            return 2;
        }
    };

    if args.invert_match {
        return handle_output_code(super::render::render_invert_match(
            &config, &results, args,
        ));
    }

    if results.is_empty() {
        return 1;
    }

    if args.quiet {
        return 0;
    }

    if args.files_with_matches {
        let stdout = io::stdout();
        let mut out = stdout.lock();
        let mut seen = std::collections::BTreeSet::new();
        for m in &results {
            seen.insert(m.path.clone());
        }
        for path in &seen {
            let result = out
                .write_all(path_bytes(path).as_ref())
                .and_then(|_| out.write_all(b"\n"));
            if let Err(err) = result {
                return handle_output(err);
            }
        }
        return 0;
    }

    if args.count {
        let stdout = io::stdout();
        let mut out = stdout.lock();
        let mut counts: std::collections::BTreeMap<PathBuf, usize> =
            std::collections::BTreeMap::new();
        for m in &results {
            *counts.entry(m.path.clone()).or_default() += 1;
        }
        for (path, n) in &counts {
            let result = out
                .write_all(path_bytes(path).as_ref())
                .and_then(|_| writeln!(out, ":{n}"));
            if let Err(err) = result {
                return handle_output(err);
            }
        }
        return 0;
    }

    let has_context = args.after_context > 0 || args.before_context > 0;

    let render = if args.json {
        super::render::render_json(&results)
    } else if has_context {
        super::render::render_with_context(&config, &results, args)
    } else if args.heading {
        super::render::render_heading(&results, args)
    } else {
        super::render::render_flat(&results, args)
    };

    if let Err(err) = render {
        return handle_output(err);
    }

    0
}

fn handle_output_code(result: io::Result<i32>) -> i32 {
    match result {
        Ok(code) => code,
        Err(err) => handle_output(err),
    }
}

fn handle_output(err: io::Error) -> i32 {
    if err.kind() == io::ErrorKind::BrokenPipe {
        0
    } else {
        eprintln!("st: {err}");
        2
    }
}

pub(super) fn build_effective_pattern(args: &SearchArgs) -> String {
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

pub(super) fn run_search(
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

pub(super) fn paths_to_glob(paths: &[PathBuf]) -> Option<String> {
    let first = paths.first()?;
    let s = first.to_string_lossy();
    if first.is_dir() {
        Some(format!("{s}/**"))
    } else {
        Some(s.into_owned())
    }
}
