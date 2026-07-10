//! CLI entry point: `st <pattern>`, `st index`, `st status`, `st update`.
//!
//! Uses clap derive for argument parsing. Output format is grep-compatible
//! by default, with `--json` for machine-readable output.

/// Command-line argument structures and CLI specifications.
pub mod args;
mod bench;
mod catchup;
mod commands;
mod config;
mod fallback;
mod init;
mod manage;
mod post_filter;
mod render;
mod scope;
mod search;
mod search_args;
#[cfg(feature = "symbols")]
mod sym;

use std::path::PathBuf;

use clap::Parser;

pub use args::{Cli, ManageCommand};
use bench::cmd_bench_search;
use config::resolve_config;
#[cfg(test)]
use config::{clamp_max_file_size, overlaps_sensitive_prefix, MAX_FILE_SIZE_CEILING};
use init::{cmd_agent, cmd_init};
use manage::{cmd_index, cmd_status, cmd_type_list, cmd_update, cmd_verify};
use scope::cmd_files;
use search::{cmd_search, SearchArgs};

/// Run the CLI. Returns the process exit code.
pub fn run() -> i32 {
    let cli = Cli::parse();

    match &cli.command {
        Some(ManageCommand::Init(args)) => return cmd_init(args),
        Some(ManageCommand::Agent { command }) => return cmd_agent(command),
        Some(ManageCommand::Hook { target }) => return crate::hook::protocols::cmd_hook(target),
        Some(ManageCommand::Rewrite { cwd, command }) => {
            return crate::hook::core::rewrite::cmd_rewrite(command, cwd.as_deref());
        }
        _ => {}
    }

    let mut config = resolve_config(&cli);
    config.verbose = cli.verbose || cli.compat.debug;

    match cli.command {
        Some(ManageCommand::Index {
            force,
            stats,
            quiet,
            recalibrate,
        }) => {
            config.recalibrate = recalibrate;
            cmd_index(config, force, stats, quiet)
        }
        Some(ManageCommand::Status { json }) => cmd_status(config, json),
        Some(ManageCommand::Verify) => cmd_verify(config),
        Some(ManageCommand::Update { flush, quiet }) => cmd_update(config, flush, quiet),
        Some(ManageCommand::Init(_))
        | Some(ManageCommand::Agent { .. })
        | Some(ManageCommand::Hook { .. })
        | Some(ManageCommand::Rewrite { .. }) => unreachable!("handled before config resolution"),
        Some(ManageCommand::BenchSearch {
            queries,
            iterations,
            warmups,
        }) => cmd_bench_search(config, &queries, iterations, warmups),
        None => {
            // --type-list and --files do not require a pattern.
            if cli.type_list {
                return cmd_type_list();
            }
            if cli.files {
                return cmd_files(config, &cli);
            }

            // When -e/--regexp supplies the pattern, clap still assigns the
            // first positional to `pattern` (it doesn't know it's a path).
            // Shift that positional into `paths` so `st -e "pat" dir` works
            // like ripgrep.  Multiple -e values are OR-combined with `|`.
            //
            // -F (fixed strings) interaction with multiple -e: each pattern
            // must be escaped INDIVIDUALLY before joining. Escaping the joined
            // `(?:a)|(?:b)` would search for that literal string instead of
            // `a` OR `b`. For multi-e under -F we escape each alternative and
            // clear `fixed_strings` on the resulting SearchArgs, because the
            // combined string is already a valid regex and re-escaping in
            // `build_effective_pattern` would corrupt it. For a single -e (or
            // no -e) under -F, `fixed_strings` stays set so
            // `build_effective_pattern` escapes it (preserving the shared
            // -w/-x wrapping path).
            let globs = cli.combined_globs();
            // True when multiple -e patterns under -F have already been escaped
            // and joined into `pattern` below; used to suppress the second
            // escaping pass in `build_effective_pattern`.
            let multi_e_fixed = cli.regexp.len() > 1 && cli.fixed_strings;
            let (pattern, paths): (String, Vec<PathBuf>) = if !cli.regexp.is_empty() {
                let mut p = cli.paths;
                if let Some(pos) = cli.pattern {
                    p.insert(0, PathBuf::from(pos));
                }
                if cli.regexp.len() == 1 {
                    // Single -e: leave raw; build_effective_pattern handles -F escape.
                    (cli.regexp.into_iter().next().unwrap(), p)
                } else if cli.fixed_strings {
                    // Multiple -e under -F: escape each alternative before joining.
                    let combined = cli
                        .regexp
                        .iter()
                        .map(|r| format!("(?:{})", regex::escape(r)))
                        .collect::<Vec<_>>()
                        .join("|");
                    (combined, p)
                } else {
                    let combined = cli
                        .regexp
                        .iter()
                        .map(|r| format!("(?:{r})"))
                        .collect::<Vec<_>>()
                        .join("|");
                    (combined, p)
                }
            } else {
                // --sym/--refs name their target directly, so no content pattern
                // is required when either is set (any given pattern is ignored).
                #[cfg(feature = "symbols")]
                let name_only = cli.sym.is_some() || cli.refs.is_some();
                #[cfg(not(feature = "symbols"))]
                let name_only = false;
                match cli.pattern {
                    Some(pat) => (pat, cli.paths),
                    None => {
                        if name_only {
                            (String::new(), cli.paths)
                        } else {
                            eprintln!("st: a pattern is required (try `st --help`)");
                            return 2;
                        }
                    }
                }
            };
            // `fixed_strings` was already applied above for the multi-e case:
            // clear it so build_effective_pattern does not re-escape the regex.
            let fixed_strings = cli.fixed_strings && !multi_e_fixed;

            // --pcre2 is not supported; warn and continue with default engine.
            if cli.compat.pcre2 {
                eprintln!("st: --pcre2 is not supported; using default regex engine");
            }

            // Flags that filter the result set but are not yet implemented.
            // Warn so callers (including agents) know their filter was dropped.
            if let Some(ref glob) = cli.compat.iglob {
                eprintln!(
                    "st: --iglob '{glob}' is not implemented; results may include excluded paths (use -g '!{glob}' for negation)"
                );
            }
            if let Some(ref pf) = cli.compat.pattern_file {
                eprintln!(
                    "st: -f/--file '{}' is not implemented; no patterns were read from that file",
                    pf.display()
                );
                // Without a pattern from the file there is nothing to search.
                // Return 2 (error) so the caller can diagnose the issue rather
                // than silently returning zero matches.
                return 2;
            }
            if cli.compat.multiline {
                eprintln!(
                    "st: --multiline (-U) is not supported; patterns containing \\n will not match across lines"
                );
            }
            if let Some(ref mfs) = cli.compat.max_filesize {
                eprintln!(
                    "st: --max-filesize '{mfs}' is not implemented; file-size filtering is skipped"
                );
            }
            if let Some(ref ig) = cli.compat.ignore_file {
                eprintln!(
                    "st: --ignore-file '{}' is not implemented; ignore rules from that file are skipped",
                    ig.display()
                );
            }
            if !cli.colors.is_empty() {
                eprintln!(
                    "st: --colors is not implemented; default match/path/line colors are used"
                );
            }

            // --smart-case: case-insensitive if the pattern has no uppercase
            // LITERAL characters.
            //
            // Compatibility note (ripgrep divergence): we scan every char in
            // `pattern`, so regex metacharacters that happen to be uppercase
            // class shorthands — `\S`, `\D`, `\W`, or ranges like `[A-Z]` —
            // count as "has uppercase" and force case-sensitive mode, even
            // though they carry no literal casing. ripgrep's smart-case inspects
            // only the literal characters of the parsed regex HIR, so
            // `rg -S '\Sfoo'` stays case-insensitive. The practical impact is
            // narrow (patterns mixing class shorthands with smart-case are
            // rare), and a faithful fix requires HIR inspection; tracked as a
            // known compatibility gap rather than silently diverging.
            let ignore_case = if cli.smart_case && !cli.case_sensitive && !cli.ignore_case {
                !pattern.chars().any(|c| c.is_uppercase())
            } else {
                cli.ignore_case
            };

            // --pretty implies --heading --line-number --color=always. The color
            // half is resolved here (auto = tty-gated otherwise); an explicit
            // `--color=never` still wins, so `--pretty --color=never` is plain.
            let heading = cli.heading || cli.pretty;
            let line_number = cli.line_number > 0 || cli.pretty;
            let color =
                render::resolve_color(render::ColorWhen::parse(cli.color.as_deref()), cli.pretty);

            let ctx = cli.context.unwrap_or(0);
            let search_args = SearchArgs {
                pattern,
                paths,
                fixed_strings,
                ignore_case,
                word_regexp: cli.word_regexp,
                line_regexp: cli.line_regexp,
                line_number,
                with_filename: cli.with_filename,
                invert_match: cli.invert_match,
                files_with_matches: cli.files_with_matches,
                files_without_match: cli.files_without_match,
                count: cli.count,
                count_matches: cli.count_matches,
                max_count: cli.max_count,
                quiet: cli.quiet,
                only_matching: cli.only_matching,
                json: cli.json,
                heading,
                color,
                no_line_number: cli.no_line_number > 0,
                no_filename: cli.no_filename,
                after_context: cli.after_context.unwrap_or(ctx),
                before_context: cli.before_context.unwrap_or(ctx),
                file_types: cli.file_type,
                type_nots: cli.type_not,
                globs,
                column: cli.column || cli.vimgrep,
                vimgrep: cli.vimgrep,
                replace: cli.replace,
                null: cli.null,
                context_separator: cli.context_separator,
                byte_offset: cli.byte_offset,
                trim: cli.trim,
                max_columns: cli.max_columns,
                search_stats: cli.search_stats,
                max_depth: cli.max_depth,
                fallback: cli.fallback,
                // The --sym/--sym-kind flags only exist when the symbols feature
                // is built; without it these stay None so no content pattern is
                // ever rerouted to symbol search.
                #[cfg(feature = "symbols")]
                sym: cli.sym,
                #[cfg(not(feature = "symbols"))]
                sym: None,
                #[cfg(feature = "symbols")]
                sym_kind: cli.sym_kind,
                #[cfg(not(feature = "symbols"))]
                sym_kind: None,
                #[cfg(feature = "symbols")]
                refs: cli.refs,
                #[cfg(not(feature = "symbols"))]
                refs: None,
            };
            cmd_search(config, &search_args)
        }
    }
}

#[cfg(test)]
mod tests;
