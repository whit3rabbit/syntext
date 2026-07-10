use std::path::PathBuf;

use crate::hook::vendors::{AgentAction, InstallScope};
use crate::cli::commands::{AgentCommand, AgentScope, InitArgs};

pub(super) fn cmd_init(args: &InitArgs) -> i32 {
    if args.fsmonitor {
        cmd_init_fsmonitor();
    }
    let agent = match resolve_init_agent(args) {
        Ok(agent) => agent,
        Err(err) => {
            eprintln!("{err}");
            return 2;
        }
    };
    let scope = resolve_init_scope(args, &agent);
    let code = crate::hook::vendors::cmd_agent(AgentAction::Install, &agent, scope);
    if code != 0 {
        return code;
    }
    if args.githooks {
        // Git hooks are a project-local integration; ignore --global for this leg.
        return crate::hook::vendors::cmd_agent(
            AgentAction::Install,
            "githooks",
            InstallScope::Project,
        );
    }
    code
}

/// Handle `st init --fsmonitor`: opt-in `git config core.fsmonitor true` in
/// the enclosing git repository. This is the only caller of
/// `freshness::enable_fsmonitor`; the bounded auto-update tip
/// (`maybe_print_fsmonitor_tip`) never sets config itself, only suggests it.
/// Best-effort: a failure here (non-git directory, no git binary) is
/// reported but does not abort the rest of `st init`.
fn cmd_init_fsmonitor() {
    let git = crate::git_util::resolve_git_binary();
    let repo_root = super::config::detect_repo_root().unwrap_or_else(|| PathBuf::from("."));
    if crate::index::freshness::enable_fsmonitor(&repo_root, &git) {
        println!("st: enabled core.fsmonitor for this repository");
    } else {
        eprintln!("st: failed to set core.fsmonitor (not a git repository, or git config failed)");
    }
}

pub(super) fn resolve_init_agent(args: &InitArgs) -> Result<String, String> {
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

pub(super) fn resolve_init_scope(args: &InitArgs, agent: &str) -> InstallScope {
    if args.scope.global && agent == "copilot" {
        return InstallScope::Project;
    }
    if args.scope.global {
        InstallScope::Global
    } else {
        InstallScope::Project
    }
}

pub(super) fn cmd_agent(command: &AgentCommand) -> i32 {
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
