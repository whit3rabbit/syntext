//! Conservative shell command rewrite rules for agent hooks.

use std::path::{Path, PathBuf};

use super::shell::{self, ShellItem, Word};

pub(crate) use super::rewrite_grep::rewrite_grep_args;
pub(crate) use super::rewrite_rg::rewrite_rg_args;

/// CLI entrypoint for `st __rewrite [--cwd PATH] <command>`.
pub fn cmd_rewrite(command: &str, cwd: Option<&Path>) -> i32 {
    let fallback = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let cwd = cwd.unwrap_or(&fallback);
    match rewrite_for_cwd(command, cwd, "st") {
        Some(rewritten) => {
            println!("{rewritten}");
            0
        }
        None => 1,
    }
}

pub(crate) fn rewrite_for_cwd(command: &str, cwd: &Path, st_program: &str) -> Option<String> {
    find_index_root(cwd)?;
    rewrite_shell_command(command, st_program)
}

pub(crate) fn find_index_root(cwd: &Path) -> Option<PathBuf> {
    cwd.ancestors()
        .find(|dir| dir.join(".syntext").is_dir())
        .map(Path::to_path_buf)
}

fn rewrite_shell_command(command: &str, st_program: &str) -> Option<String> {
    let parsed = shell::parse(command).ok()?;
    if parsed.has_pipe || parsed.has_redirection || parsed.has_expansion || parsed.has_background {
        return None;
    }

    let mut changed = false;
    let mut output = Vec::new();

    for item in parsed.items {
        match item {
            ShellItem::Command(words) => match rewrite_command_words(&words, st_program) {
                SegmentRewrite::Rewritten(rendered) => {
                    changed = true;
                    output.push(rendered);
                }
                SegmentRewrite::Unchanged => output.push(shell::render_raw_words(&words)),
                SegmentRewrite::UnsupportedSearch => return None,
            },
            ShellItem::Op(op) => output.push(op),
        }
    }

    changed.then(|| output.join(" "))
}

enum SegmentRewrite {
    Rewritten(String),
    Unchanged,
    UnsupportedSearch,
}

fn rewrite_command_words(words: &[Word], st_program: &str) -> SegmentRewrite {
    let Some(command_index) = words
        .iter()
        .position(|word| !shell::is_env_assignment(&word.text))
    else {
        return SegmentRewrite::Unchanged;
    };

    let command = words[command_index].text.as_str();
    let args = &words[command_index + 1..];
    let rewritten_args = match command {
        "rg" => rewrite_rg_args(args),
        "grep" => rewrite_grep_args(args),
        _ => return SegmentRewrite::Unchanged,
    };

    match rewritten_args {
        Some(args) => SegmentRewrite::Rewritten(render_rewritten_segment(
            &words[..command_index],
            st_program,
            &args,
        )),
        None => SegmentRewrite::UnsupportedSearch,
    }
}

fn render_rewritten_segment(env: &[Word], st_program: &str, args: &[String]) -> String {
    let mut pieces: Vec<String> = env
        .iter()
        .map(|word| quote_env_assignment(&word.text))
        .collect();
    pieces.push(shell::shell_quote(st_program));
    pieces.extend(args.iter().map(|arg| shell::shell_quote(arg)));
    pieces.join(" ")
}

/// Re-quote the VALUE of a leading `KEY=VALUE` env assignment so any
/// shell-active characters in the value are neutralized when the rewritten
/// command is re-executed by a shell.
///
/// These env words are the only tokens re-emitted into the rewritten command
/// from parsed input; every other value already passes through
/// [`shell::shell_quote`]. Emitting the original `.raw` verbatim (the prior
/// behavior) could reintroduce expansion the parse-time `has_expansion` gate
/// does not cover inside an assignment value (globs, brace/tilde expansion,
/// re-opened quotes). Re-quoting the value from the shell-parsed `.text`
/// (quotes already removed) makes the emitted assignment inert while preserving
/// its meaning. The KEY is guaranteed well-formed by `is_env_assignment`
/// upstream; the `None` arm is defensive.
pub(crate) fn quote_env_assignment(text: &str) -> String {
    match text.split_once('=') {
        Some((key, value)) => format!("{key}={}", shell::shell_quote(value)),
        None => shell::shell_quote(text),
    }
}



// rewrite_tests.rs is loaded as a sibling module via `mod rewrite_tests;` in
// hook/core/mod.rs (its tests use absolute `crate::hook::core::...` paths), so
// it is NOT re-declared here -- doing so would load the same file as two
// distinct modules (clippy::duplicate_mod).
