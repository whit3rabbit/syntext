//! Search argument parsing, query execution, and result rendering.

use std::path::PathBuf;

use crate::index::Index;
use crate::{Config, SearchOptions};

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
        return super::render::render_invert_match(&config, &results, args);
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
        super::render::render_json(&config, &results);
    } else if has_context {
        super::render::render_with_context(&config, &results, args);
    } else if args.heading {
        super::render::render_heading(&results, args);
    } else {
        super::render::render_flat(&results, args);
    }

    0
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
    if paths.len() > 1 {
        eprintln!(
            "rl: warning: multiple path arguments not yet supported; using only {:?}",
            paths[0]
        );
    }
    let first = paths.first()?;
    let s = first.to_string_lossy();
    if first.is_dir() {
        Some(format!("{s}/**"))
    } else {
        Some(s.into_owned())
    }
}
