//! Management subcommand definitions for the top-level CLI.

use std::path::PathBuf;

use clap::{Args, Subcommand};

/// Scope selector shared by agent install/show/uninstall commands.
#[derive(Args, Debug, Clone, Copy)]
pub struct AgentScope {
    /// Install in the agent's global configuration.
    #[arg(short = 'g', long, conflicts_with = "project")]
    pub global: bool,

    /// Install in the current project's agent configuration.
    #[arg(long, conflicts_with = "global")]
    pub project: bool,
}

/// RTK-style convenience installer arguments.
#[derive(Args, Debug)]
pub struct InitArgs {
    #[command(flatten)]
    pub scope: AgentScope,

    /// Agent integration name.
    #[arg(long, value_name = "AGENT")]
    pub agent: Option<String>,

    /// Install the Claude Code integration.
    #[arg(long)]
    pub claude: bool,

    /// Install the Cursor integration.
    #[arg(long)]
    pub cursor: bool,

    /// Install the GitHub Copilot integration.
    #[arg(long)]
    pub copilot: bool,

    /// Install the Gemini CLI integration.
    #[arg(long)]
    pub gemini: bool,

    /// Install the OpenCode integration.
    #[arg(long)]
    pub opencode: bool,

    /// Install the OpenClaw integration.
    #[arg(long)]
    pub openclaw: bool,

    /// Install the Codex CLI integration.
    #[arg(long)]
    pub codex: bool,

    /// Install the Cline / Roo Code rules integration.
    #[arg(long)]
    pub cline: bool,

    /// Install the Windsurf rules integration.
    #[arg(long)]
    pub windsurf: bool,

    /// Install the Kilo Code rules integration.
    #[arg(long)]
    pub kilocode: bool,

    /// Install the Google Antigravity rules integration.
    #[arg(long)]
    pub antigravity: bool,
}

/// Agent integration subcommands.
#[derive(Subcommand, Debug)]
pub enum AgentCommand {
    /// Install an agent integration.
    Install {
        /// Agent integration name.
        agent: String,
        #[command(flatten)]
        scope: AgentScope,
    },
    /// Uninstall an agent integration.
    Uninstall {
        /// Agent integration name.
        agent: String,
        #[command(flatten)]
        scope: AgentScope,
    },
    /// Show agent integration status.
    Show {
        /// Agent integration name.
        agent: String,
        #[command(flatten)]
        scope: AgentScope,
    },
}

/// Management subcommands dispatched from the top-level CLI.
#[derive(Subcommand, Debug)]
pub enum ManageCommand {
    /// Build or rebuild the search index.
    Index {
        /// Rebuild from scratch even if an index exists.
        #[arg(long)]
        force: bool,
        /// Print statistics after build.
        #[arg(long)]
        stats: bool,
        /// Suppress progress output.
        #[arg(short, long)]
        quiet: bool,
    },
    /// Show index statistics.
    Status {
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Incrementally update the index for changed files.
    Update {
        /// Force flush overlay to segment.
        #[arg(long)]
        flush: bool,
        /// Suppress output.
        #[arg(short, long)]
        quiet: bool,
    },
    /// Install an agent integration using RTK-style flags.
    Init(InitArgs),
    /// Manage agent integrations.
    Agent {
        #[command(subcommand)]
        command: AgentCommand,
    },
    /// Internal hook entrypoint for agent integrations.
    #[command(name = "__hook", hide = true)]
    Hook {
        /// Hook target name.
        target: String,
    },
    /// Internal command rewrite helper.
    #[command(name = "__rewrite", hide = true)]
    Rewrite {
        /// Directory used to decide whether a .syntext index is present.
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Shell command to rewrite.
        command: String,
    },
    #[command(hide = true)]
    BenchSearch {
        #[arg(long = "query", required = true)]
        queries: Vec<String>,
        #[arg(long, default_value_t = 1)]
        iterations: usize,
        #[arg(long, default_value_t = 0)]
        warmups: usize,
    },
}
