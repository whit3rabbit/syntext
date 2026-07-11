use std::io::{self, Write};
use std::path::PathBuf;

use crate::index::Index;
use crate::path_util::path_bytes;
use crate::Config;

use super::{
    explicit_path_specs, matches_any_explicit_path, matches_optional_glob, path_depth,
    CompiledGlobs,
};

/// List indexed files matching type/glob filters (--files mode).
pub(crate) fn cmd_files(config: Config, cli: &crate::cli::args::Cli) -> i32 {
    // Reject malformed -g/--glob specs up front, mirroring `cmd_search`. Without
    // this, a bad glob degrades to a silent never-match filter (the file is
    // simply omitted from the listing) instead of exiting 2, and a malformed
    // negative glob silently drops an exclusion.
    let globs = cli.combined_globs();
    if let Err((spec, msg)) = super::validate_globs(&globs) {
        eprintln!("st: invalid glob '{spec}': {msg}");
        return 2;
    }
    // Compile globs once, not once per candidate path.
    let compiled_globs = CompiledGlobs::build(&globs);

    let index = match Index::open(config.clone()) {
        Ok(idx) => idx,
        Err(e) => {
            eprintln!("st: {e}");
            return 2;
        }
    };

    // Bounded auto-update: keep `--files` output consistent with `st <pattern>`
    // search results, so a freshly created file is listed here too instead of
    // only showing up once a manual `st update` runs. See
    // `catchup::run_bounded_auto_update` for the full error-handling contract.
    // Same notice/quiet gating as `cmd_search`: still-stale-after-update spawns
    // the detached async catch-up, regardless of whether the stderr notice
    // itself was suppressed by `--quiet`.
    let needs_async_catchup =
        crate::cli::catchup::run_bounded_auto_update(&index, &config, cli.quiet);

    let snapshot = index.snapshot();
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let sep = if cli.null { b'\0' } else { b'\n' };

    // Scope by the requested path(s), matching `rg --files <path>`. Dispatch to
    // cmd_files happens before the `-e` positional shift, so the first positional
    // lands in `cli.pattern` (not `cli.paths`) for `st --files src/`; fold both
    // in. `explicit_path_specs` drops "." / repo-root specs, and
    // `matches_any_explicit_path` returns true on empty specs, so no path given
    // still lists everything.
    let mut path_args: Vec<PathBuf> = Vec::new();
    if let Some(pat) = &cli.pattern {
        path_args.push(PathBuf::from(pat));
    }
    path_args.extend(cli.paths.iter().cloned());
    let specs = explicit_path_specs(config.repo_root.as_path(), &path_args);

    let mut paths: Vec<_> = snapshot
        .path_index
        .visible_paths()
        .filter(|(_, path)| {
            matches_any_explicit_path(path, &specs)
                && matches_optional_glob(path, &cli.file_type, &cli.type_not, &compiled_globs)
        })
        .map(|(_, path)| path.to_path_buf())
        .collect();
    if let Some(depth) = cli.max_depth {
        // rg --max-depth is relative to each search path, not the repo root.
        // Mirror the same logic used in run_search.
        if specs.is_empty() {
            paths.retain(|p| path_depth(p) <= depth);
        } else {
            paths.retain(|p| {
                let spec_depth = specs
                    .iter()
                    .filter(|spec| {
                        spec.rel_path.as_os_str().is_empty() || p.starts_with(&spec.rel_path)
                    })
                    .map(|spec| spec.rel_path.components().count())
                    .max()
                    .unwrap_or(0);
                path_depth(p).saturating_sub(spec_depth) <= depth
            });
        }
    }
    paths.sort_unstable();
    let mut exit_code = 0;
    for path in &paths {
        let result = out
            .write_all(path_bytes(path).as_ref())
            .and_then(|_| out.write_all(&[sep]));
        if let Err(err) = result {
            if err.kind() != io::ErrorKind::BrokenPipe {
                eprintln!("st: {err}");
                exit_code = 2;
            }
            break;
        }
    }

    // Spawn the async catch-up only after output is done, so the extra
    // process never delays or reorders this command's own stdout/stderr.
    if needs_async_catchup {
        crate::cli::catchup::maybe_spawn_async_catchup(&config);
    }

    exit_code
}
