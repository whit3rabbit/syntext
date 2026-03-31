//! Search argument parsing, query execution, and result rendering.

use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::time::Instant;

use crate::index::Index;
use crate::path_util::path_bytes;
use crate::Config;

// Re-export for render submodules that import via `crate::cli::search::collect_scoped_paths`.
pub(super) use super::scope::collect_scoped_paths;
use super::scope::{
    explicit_path_specs, matches_any_explicit_path, matches_optional_glob, path_depth,
    search_options, shows_filename_by_default, sort_and_dedup_matches, truncate_matches_per_file,
};

#[derive(Clone)]
pub(super) struct SearchArgs {
    pub pattern: String,
    pub paths: Vec<PathBuf>,
    pub fixed_strings: bool,
    pub ignore_case: bool,
    pub word_regexp: bool,
    pub line_regexp: bool,
    pub line_number: bool,
    pub with_filename: bool,
    pub invert_match: bool,
    pub files_with_matches: bool,
    pub files_without_match: bool,
    pub count: bool,
    pub count_matches: bool,
    pub max_count: Option<usize>,
    pub quiet: bool,
    pub only_matching: bool,
    pub json: bool,
    pub heading: bool,
    pub no_line_number: bool,
    pub no_filename: bool,
    pub after_context: usize,
    pub before_context: usize,
    pub file_type: Option<String>,
    pub type_not: Option<String>,
    pub glob: Option<String>,
    pub column: bool,
    pub vimgrep: bool,
    pub replace: Option<String>,
    pub null: bool,
    pub context_separator: String,
    pub byte_offset: bool,
    pub trim: bool,
    pub max_columns: Option<usize>,
    pub search_stats: bool,
    pub max_depth: Option<usize>,
}

impl Default for SearchArgs {
    fn default() -> Self {
        Self {
            pattern: String::new(),
            paths: Vec::new(),
            fixed_strings: false,
            ignore_case: false,
            word_regexp: false,
            line_regexp: false,
            line_number: false,
            with_filename: false,
            invert_match: false,
            files_with_matches: false,
            files_without_match: false,
            count: false,
            count_matches: false,
            max_count: None,
            quiet: false,
            only_matching: false,
            json: false,
            heading: false,
            no_line_number: false,
            no_filename: false,
            after_context: 0,
            before_context: 0,
            file_type: None,
            type_not: None,
            glob: None,
            column: false,
            vimgrep: false,
            replace: None,
            null: false,
            context_separator: "--".to_string(),
            byte_offset: false,
            trim: false,
            max_columns: None,
            search_stats: false,
            max_depth: None,
        }
    }
}

pub(super) fn cmd_search(config: Config, args: &SearchArgs) -> i32 {
    let index = match Index::open(config.clone()) {
        Ok(idx) => idx,
        Err(e) => {
            eprintln!("st: {e}");
            return 2;
        }
    };
    let output_args = args.with_effective_output_defaults(&config);
    let search_start = Instant::now();

    if output_args.invert_match {
        return handle_output_code(super::render::render_invert_match(
            &index,
            &config,
            &output_args,
        ));
    }

    let results = match run_search(&index, &config, args) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("st: {e}");
            return 2;
        }
    };
    let elapsed = search_start.elapsed();
    if output_args.search_stats {
        let matched_files: std::collections::BTreeSet<_> =
            results.iter().map(|m| &m.path).collect();
        eprintln!(
            "Elapsed: {:.6}s, Matches: {}, Files with matches: {}",
            elapsed.as_secs_f64(),
            results.len(),
            matched_files.len()
        );
    }

    if results.is_empty() && output_args.json {
        if let Err(err) = super::render::render_json(&index, &config, &results, &output_args) {
            return handle_output(err);
        }
        return 1;
    }

    if results.is_empty() {
        return 1;
    }

    if output_args.quiet {
        return 0;
    }

    if output_args.files_with_matches {
        let stdout = io::stdout();
        let mut out = stdout.lock();
        let sep = if output_args.null { b'\0' } else { b'\n' };
        let mut seen = std::collections::BTreeSet::new();
        for m in &results {
            seen.insert(m.path.clone());
        }
        for path in &seen {
            let result = out
                .write_all(path_bytes(path).as_ref())
                .and_then(|_| out.write_all(&[sep]));
            if let Err(err) = result {
                return handle_output(err);
            }
        }
        return 0;
    }

    if output_args.files_without_match {
        let stdout = io::stdout();
        let mut out = stdout.lock();
        let sep = if output_args.null { b'\0' } else { b'\n' };
        let matched: std::collections::BTreeSet<_> =
            results.iter().map(|m| m.path.clone()).collect();
        let mut found_any = false;
        for path in collect_scoped_paths(&index, &config, &output_args) {
            if matched.contains(&path) {
                continue;
            }
            found_any = true;
            let result = out
                .write_all(path_bytes(&path).as_ref())
                .and_then(|_| out.write_all(&[sep]));
            if let Err(err) = result {
                return handle_output(err);
            }
        }
        return if found_any { 0 } else { 1 };
    }

    if output_args.count_matches {
        return handle_output_code(super::render::render_count_matches(
            &config,
            &results,
            &output_args,
        ));
    }

    if output_args.count && output_args.only_matching {
        return handle_output_code(super::render::render_count_matches(
            &config,
            &results,
            &output_args,
        ));
    }

    if output_args.count {
        let stdout = io::stdout();
        let mut out = stdout.lock();
        let mut counts: std::collections::BTreeMap<PathBuf, usize> =
            std::collections::BTreeMap::new();
        for m in &results {
            *counts.entry(m.path.clone()).or_default() += 1;
        }
        for (path, n) in &counts {
            let result = if output_args.no_filename {
                writeln!(out, "{n}")
            } else {
                out.write_all(path_bytes(path).as_ref())
                    .and_then(|_| writeln!(out, ":{n}"))
            };
            if let Err(err) = result {
                return handle_output(err);
            }
        }
        return 0;
    }

    let has_context = output_args.after_context > 0 || output_args.before_context > 0;

    let render = if output_args.json {
        super::render::render_json(&index, &config, &results, &output_args)
    } else if output_args.vimgrep {
        super::render::render_vimgrep(&config, &results, &output_args)
    } else if output_args.only_matching {
        super::render::render_only_matching(&config, &results, &output_args)
    } else if has_context {
        super::render::render_with_context(&config, &results, &output_args)
    } else if output_args.heading {
        super::render::render_heading(&results, &output_args)
    } else {
        super::render::render_flat(&results, &output_args)
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
    if args.line_regexp {
        format!("^(?:{pat})$")
    } else if args.word_regexp {
        format!(r"\b{pat}\b")
    } else {
        pat
    }
}

pub(super) fn run_search(
    index: &Index,
    config: &Config,
    args: &SearchArgs,
) -> Result<Vec<crate::SearchMatch>, crate::IndexError> {
    let effective_pattern = build_effective_pattern(args);
    let explicit_specs = explicit_path_specs(&config.repo_root, &args.paths);
    let mut results = if explicit_specs.is_empty() {
        index.search(&effective_pattern, &search_options(args, args.glob.clone()))?
    } else {
        let mut merged = Vec::new();
        for spec in &explicit_specs {
            merged.extend(index.search(
                &effective_pattern,
                &search_options(args, Some(spec.path_filter())),
            )?);
        }
        sort_and_dedup_matches(merged)
    };
    if !explicit_specs.is_empty() || args.glob.is_some() {
        results.retain(|m| {
            matches_any_explicit_path(&m.path, &explicit_specs)
                && matches_optional_glob(
                    &m.path,
                    args.file_type.as_deref(),
                    args.type_not.as_deref(),
                    args.glob.as_deref(),
                )
        });
    }
    if let Some(depth) = args.max_depth {
        results.retain(|m| path_depth(&m.path) <= depth);
    }
    if let Some(limit) = args.max_count {
        results = truncate_matches_per_file(results, limit);
    }
    Ok(results)
}

impl SearchArgs {
    fn with_effective_output_defaults(&self, config: &Config) -> Self {
        let mut effective = self.clone();
        let stdout_is_tty = io::stdout().is_terminal();

        if !self.line_number && !self.no_line_number {
            effective.no_line_number = !stdout_is_tty;
        }

        if self.with_filename {
            effective.no_filename = false;
        } else if !self.no_filename {
            effective.no_filename = !shows_filename_by_default(config, &self.paths);
        }

        effective
    }
}
