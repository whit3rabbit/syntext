//! CLI entry point: `st <pattern>`, `st index`, `st status`, `st update`.
//!
//! Uses clap derive for argument parsing. Output format is grep-compatible
//! by default, with `--json` for machine-readable output.

pub mod args;
mod bench;
mod commands;
mod config;
mod fallback;
mod manage;
mod render;
mod scope;
mod search;

use std::path::PathBuf;

use clap::Parser;

use crate::hook::vendors::{AgentAction, InstallScope};
pub use args::{Cli, ManageCommand};
use bench::cmd_bench_search;
use commands::{AgentCommand, AgentScope, InitArgs};
#[cfg(test)]
use config::{clamp_max_file_size, overlaps_sensitive_prefix, MAX_FILE_SIZE_CEILING};
use config::resolve_config;
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
    config.verbose = cli.verbose || cli.debug;

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
            let globs = cli.combined_globs();
            let (pattern, paths) = if !cli.regexp.is_empty() {
                let mut p = cli.paths;
                if let Some(pos) = cli.pattern {
                    p.insert(0, PathBuf::from(pos));
                }
                let combined = if cli.regexp.len() == 1 {
                    cli.regexp.into_iter().next().unwrap()
                } else {
                    cli.regexp
                        .iter()
                        .map(|r| format!("(?:{r})"))
                        .collect::<Vec<_>>()
                        .join("|")
                };
                (combined, p)
            } else {
                match cli.pattern {
                    Some(pat) => (pat, cli.paths),
                    None => {
                        eprintln!("st: a pattern is required (try `st --help`)");
                        return 2;
                    }
                }
            };

            // --pcre2 is not supported; warn and continue with default engine.
            if cli.pcre2 {
                eprintln!("st: --pcre2 is not supported; using default regex engine");
            }

            // --smart-case: case-insensitive if pattern has no uppercase chars.
            let ignore_case = if cli.smart_case && !cli.case_sensitive && !cli.ignore_case {
                !pattern.chars().any(|c| c.is_uppercase())
            } else {
                cli.ignore_case
            };

            // --pretty is an alias for --heading --line-number (color is no-op).
            let heading = cli.heading || cli.pretty;
            let line_number = cli.line_number > 0 || cli.pretty;

            let ctx = cli.context.unwrap_or(0);
            let search_args = SearchArgs {
                pattern,
                paths,
                fixed_strings: cli.fixed_strings,
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
            };
            cmd_search(config, &search_args)
        }
    }
}

fn cmd_init(args: &InitArgs) -> i32 {
    let agent = match resolve_init_agent(args) {
        Ok(agent) => agent,
        Err(err) => {
            eprintln!("{err}");
            return 2;
        }
    };
    let scope = resolve_init_scope(args, &agent);
    crate::hook::vendors::cmd_agent(AgentAction::Install, &agent, scope)
}

fn resolve_init_agent(args: &InitArgs) -> Result<String, String> {
    let selected = [
        ("claude", args.claude),
        ("cursor", args.cursor),
        ("copilot", args.copilot),
        ("gemini", args.gemini),
        ("opencode", args.opencode),
        ("openclaw", args.openclaw),
        ("codex", args.codex),
        ("cline", args.cline),
        ("windsurf", args.windsurf),
        ("kilocode", args.kilocode),
        ("antigravity", args.antigravity),
    ]
    .into_iter()
    .filter_map(|(name, enabled)| enabled.then_some(name))
    .collect::<Vec<_>>();

    match (args.agent.as_deref(), selected.as_slice()) {
        (Some(_), [_first, ..]) => {
            Err("st: choose either --agent or one agent shortcut flag, not both".to_string())
        }
        (Some(agent), []) => Ok(agent.to_string()),
        (None, []) => Ok("claude".to_string()),
        (None, [agent]) => Ok((*agent).to_string()),
        (None, [..]) => Err("st: choose only one agent shortcut flag".to_string()),
    }
}

fn resolve_init_scope(args: &InitArgs, agent: &str) -> InstallScope {
    if args.scope.global && agent == "copilot" {
        return InstallScope::Project;
    }
    if args.scope.global {
        InstallScope::Global
    } else {
        InstallScope::Project
    }
}

fn cmd_agent(command: &AgentCommand) -> i32 {
    match command {
        AgentCommand::Install { agent, scope } => {
            let Some(scope) = resolve_agent_scope(scope) else {
                return 2;
            };
            crate::hook::vendors::cmd_agent(AgentAction::Install, agent, scope)
        }
        AgentCommand::Uninstall { agent, scope } => {
            let Some(scope) = resolve_agent_scope(scope) else {
                return 2;
            };
            crate::hook::vendors::cmd_agent(AgentAction::Uninstall, agent, scope)
        }
        AgentCommand::Show { agent, scope } => {
            let Some(scope) = resolve_agent_scope(scope) else {
                return 2;
            };
            crate::hook::vendors::cmd_agent(AgentAction::Show, agent, scope)
        }
    }
}

fn resolve_agent_scope(scope: &AgentScope) -> Option<InstallScope> {
    match (scope.global, scope.project) {
        (true, false) => Some(InstallScope::Global),
        (false, true) => Some(InstallScope::Project),
        _ => {
            eprintln!("st: choose exactly one of --global or --project");
            None
        }
    }
}

#[cfg(test)]
mod tests;
