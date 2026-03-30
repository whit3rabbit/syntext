//! Search argument parsing, query execution, and result rendering.

use std::io::{self, IsTerminal, Write};
use std::path::{Component, Path, PathBuf};

use crate::index::Index;
use crate::path::filter::matches_path_filter;
use crate::path_util::path_bytes;
use crate::{Config, SearchOptions};

#[derive(Clone, Default)]
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

    if results.is_empty() {
        return 1;
    }

    if output_args.quiet {
        return 0;
    }

    if output_args.files_with_matches {
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

    if output_args.files_without_match {
        let stdout = io::stdout();
        let mut out = stdout.lock();
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
                .and_then(|_| out.write_all(b"\n"));
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
        super::render::render_json(&results)
    } else if output_args.only_matching {
        super::render::render_only_matching(&results, &output_args)
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
    if let Some(limit) = args.max_count {
        results = truncate_matches_per_file(results, limit);
    }
    Ok(results)
}

fn truncate_matches_per_file(
    matches: Vec<crate::SearchMatch>,
    limit: usize,
) -> Vec<crate::SearchMatch> {
    let mut kept = Vec::with_capacity(matches.len().min(limit));
    let mut current_path: Option<PathBuf> = None;
    let mut kept_in_file = 0usize;

    for m in matches {
        if current_path.as_ref() != Some(&m.path) {
            current_path = Some(m.path.clone());
            kept_in_file = 0;
        }
        if kept_in_file < limit {
            kept.push(m);
            kept_in_file += 1;
        }
    }

    kept
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

#[derive(Clone)]
struct ExplicitPathSpec {
    rel_path: PathBuf,
    is_dir: bool,
}

impl ExplicitPathSpec {
    fn path_filter(&self) -> String {
        let rel = self.rel_path.to_string_lossy();
        if self.is_dir {
            format!("{rel}/")
        } else {
            rel.into_owned()
        }
    }
}

fn explicit_path_specs(repo_root: &Path, paths: &[PathBuf]) -> Vec<ExplicitPathSpec> {
    paths
        .iter()
        .map(|path| ExplicitPathSpec {
            rel_path: relativize_cli_path(repo_root, path),
            is_dir: path_is_directory(repo_root, path),
        })
        .collect()
}

fn matches_any_explicit_path(path: &Path, specs: &[ExplicitPathSpec]) -> bool {
    specs.is_empty() || specs.iter().any(|spec| explicit_path_matches(path, spec))
}

fn explicit_path_matches(path: &Path, spec: &ExplicitPathSpec) -> bool {
    if spec.rel_path.as_os_str().is_empty() {
        return true;
    }
    if spec.is_dir {
        path.starts_with(&spec.rel_path)
    } else {
        path == spec.rel_path
    }
}

fn shows_filename_by_default(config: &Config, paths: &[PathBuf]) -> bool {
    match explicit_path_specs(config.repo_root.as_path(), paths).as_slice() {
        [] => true,
        [spec] => spec.is_dir,
        _ => true,
    }
}

fn path_is_directory(repo_root: &Path, path: &Path) -> bool {
    cli_path_on_disk(repo_root, path)
        .metadata()
        .map(|meta| meta.is_dir())
        .unwrap_or(false)
}

fn relativize_cli_path(repo_root: &Path, path: &Path) -> PathBuf {
    let rel = if path.is_absolute() {
        path.strip_prefix(repo_root).unwrap_or(path)
    } else {
        path
    };
    normalize_relative_path(rel)
}

fn cli_path_on_disk(repo_root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        repo_root.join(path)
    }
}

fn normalize_relative_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => normalized.push(part),
            Component::ParentDir => normalized.push(component.as_os_str()),
            Component::RootDir | Component::Prefix(_) => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn search_options(args: &SearchArgs, path_filter: Option<String>) -> SearchOptions {
    SearchOptions {
        case_insensitive: args.ignore_case,
        file_type: args.file_type.clone(),
        exclude_type: args.type_not.clone(),
        max_results: None,
        path_filter,
    }
}

fn matches_optional_glob(
    path: &Path,
    file_type: Option<&str>,
    exclude_type: Option<&str>,
    path_glob: Option<&str>,
) -> bool {
    matches_path_filter(path, file_type, exclude_type, path_glob)
}

pub(super) fn collect_scoped_paths(
    index: &Index,
    config: &Config,
    args: &SearchArgs,
) -> Vec<PathBuf> {
    let snapshot = index.snapshot();
    let explicit_specs = explicit_path_specs(config.repo_root.as_path(), &args.paths);
    let mut paths: Vec<PathBuf> = snapshot
        .path_index
        .visible_paths()
        .filter_map(|(_, path)| {
            (matches_any_explicit_path(path, &explicit_specs)
                && matches_optional_glob(
                    path,
                    args.file_type.as_deref(),
                    args.type_not.as_deref(),
                    args.glob.as_deref(),
                ))
            .then(|| path.to_path_buf())
        })
        .collect();
    paths.sort_unstable();
    paths
}

fn sort_and_dedup_matches(mut matches: Vec<crate::SearchMatch>) -> Vec<crate::SearchMatch> {
    matches.sort_unstable_by(|a, b| {
        a.path
            .cmp(&b.path)
            .then_with(|| a.line_number.cmp(&b.line_number))
            .then_with(|| a.byte_offset.cmp(&b.byte_offset))
            .then_with(|| a.submatch_start.cmp(&b.submatch_start))
            .then_with(|| a.submatch_end.cmp(&b.submatch_end))
    });
    matches.dedup_by(|a, b| {
        a.path == b.path
            && a.line_number == b.line_number
            && a.byte_offset == b.byte_offset
            && a.submatch_start == b.submatch_start
            && a.submatch_end == b.submatch_end
    });
    matches
}
