//! Vendor-specific hook installers.

use std::path::PathBuf;

use crate::hook::core::files;

mod claude;
mod codex;
mod copilot;
mod cursor;
mod gemini;
mod openclaw;
mod opencode;
mod rules;

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallScope {
    Global,
    Project,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentAction {
    Install,
    Uninstall,
    Show,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Agent {
    Claude,
    Cursor,
    Copilot,
    Gemini,
    OpenCode,
    OpenClaw,
    Codex,
    Cline,
    Windsurf,
    KiloCode,
    Antigravity,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Outcome {
    pub(crate) installed: bool,
    pub(crate) changed: Vec<PathBuf>,
    pub(crate) removed: Vec<PathBuf>,
}

pub fn cmd_agent(action: AgentAction, agent: &str, scope: InstallScope) -> i32 {
    let agent = match Agent::parse(agent) {
        Some(agent) => agent,
        None => {
            eprintln!("st: unsupported agent '{agent}'");
            return 2;
        }
    };
    if let Err(err) = validate_scope(agent, scope) {
        eprintln!("{err}");
        return 2;
    }

    let result = match action {
        AgentAction::Install => install(agent, scope),
        AgentAction::Uninstall => uninstall(agent, scope),
        AgentAction::Show => show(agent, scope),
    };
    match result {
        Ok(outcome) => {
            print_outcome(action, agent, scope, &outcome);
            0
        }
        Err(err) => {
            eprintln!("{err}");
            1
        }
    }
}

fn install(agent: Agent, scope: InstallScope) -> Result<Outcome, String> {
    let st = files::current_st_program()?;
    match agent {
        Agent::Claude => claude::install(scope, &st),
        Agent::Cursor => cursor::install(&st),
        Agent::Copilot => copilot::install(&st),
        Agent::Gemini => gemini::install(&st),
        Agent::OpenCode => opencode::install(&st),
        Agent::OpenClaw => openclaw::install(&st),
        Agent::Codex => codex::install(scope),
        Agent::Cline => rules::install(".clinerules", "cline", "Syntext Code Search"),
        Agent::Windsurf => rules::install(".windsurfrules", "windsurf", "Syntext Code Search"),
        Agent::KiloCode => rules::install(
            ".kilocode/rules/syntext-rules.md",
            "kilocode",
            "Syntext Code Search",
        ),
        Agent::Antigravity => rules::install(
            ".agents/rules/antigravity-syntext-rules.md",
            "antigravity",
            "Syntext Code Search",
        ),
    }
}

fn uninstall(agent: Agent, scope: InstallScope) -> Result<Outcome, String> {
    match agent {
        Agent::Claude => claude::uninstall(scope),
        Agent::Cursor => cursor::uninstall(),
        Agent::Copilot => copilot::uninstall(),
        Agent::Gemini => gemini::uninstall(),
        Agent::OpenCode => opencode::uninstall(),
        Agent::OpenClaw => openclaw::uninstall(),
        Agent::Codex => codex::uninstall(scope),
        Agent::Cline => rules::uninstall(".clinerules", "cline"),
        Agent::Windsurf => rules::uninstall(".windsurfrules", "windsurf"),
        Agent::KiloCode => rules::uninstall(".kilocode/rules/syntext-rules.md", "kilocode"),
        Agent::Antigravity => {
            rules::uninstall(".agents/rules/antigravity-syntext-rules.md", "antigravity")
        }
    }
}

fn show(agent: Agent, scope: InstallScope) -> Result<Outcome, String> {
    match agent {
        Agent::Claude => claude::show(scope),
        Agent::Cursor => cursor::show(),
        Agent::Copilot => copilot::show(),
        Agent::Gemini => gemini::show(),
        Agent::OpenCode => opencode::show(),
        Agent::OpenClaw => openclaw::show(),
        Agent::Codex => codex::show(scope),
        Agent::Cline => rules::show(".clinerules", "cline"),
        Agent::Windsurf => rules::show(".windsurfrules", "windsurf"),
        Agent::KiloCode => rules::show(".kilocode/rules/syntext-rules.md", "kilocode"),
        Agent::Antigravity => {
            rules::show(".agents/rules/antigravity-syntext-rules.md", "antigravity")
        }
    }
}

fn validate_scope(agent: Agent, scope: InstallScope) -> Result<(), String> {
    let ok = match agent {
        Agent::Claude | Agent::Codex => true,
        Agent::Cursor | Agent::Gemini | Agent::OpenCode | Agent::OpenClaw => {
            scope == InstallScope::Global
        }
        Agent::Copilot | Agent::Cline | Agent::Windsurf | Agent::KiloCode | Agent::Antigravity => {
            scope == InstallScope::Project
        }
    };
    if ok {
        Ok(())
    } else {
        Err(format!(
            "st: {} supports {} scope only",
            agent.name(),
            agent.supported_scope_label()
        ))
    }
}

fn print_outcome(action: AgentAction, agent: Agent, scope: InstallScope, outcome: &Outcome) {
    if action == AgentAction::Show {
        println!(
            "{} {}: {}",
            agent.name(),
            scope.label(),
            if outcome.installed {
                "installed"
            } else {
                "missing"
            }
        );
        return;
    }
    let verb = match action {
        AgentAction::Install => "installed",
        AgentAction::Uninstall => "uninstalled",
        AgentAction::Show => unreachable!(),
    };
    println!("st: {} {} {}", agent.name(), scope.label(), verb);
}

impl Agent {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "claude" => Some(Self::Claude),
            "cursor" => Some(Self::Cursor),
            "copilot" => Some(Self::Copilot),
            "gemini" => Some(Self::Gemini),
            "opencode" => Some(Self::OpenCode),
            "openclaw" => Some(Self::OpenClaw),
            "codex" => Some(Self::Codex),
            "cline" => Some(Self::Cline),
            "windsurf" => Some(Self::Windsurf),
            "kilocode" => Some(Self::KiloCode),
            "antigravity" => Some(Self::Antigravity),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Cursor => "cursor",
            Self::Copilot => "copilot",
            Self::Gemini => "gemini",
            Self::OpenCode => "opencode",
            Self::OpenClaw => "openclaw",
            Self::Codex => "codex",
            Self::Cline => "cline",
            Self::Windsurf => "windsurf",
            Self::KiloCode => "kilocode",
            Self::Antigravity => "antigravity",
        }
    }

    fn supported_scope_label(self) -> &'static str {
        match self {
            Self::Claude | Self::Codex => "global or project",
            Self::Cursor | Self::Gemini | Self::OpenCode | Self::OpenClaw => "global",
            Self::Copilot | Self::Cline | Self::Windsurf | Self::KiloCode | Self::Antigravity => {
                "project"
            }
        }
    }
}

impl InstallScope {
    fn label(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Project => "project",
        }
    }
}
