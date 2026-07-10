//! Conservative shell command rewrite rules for agent hooks.

use std::path::{Path, PathBuf};

use super::shell::{self, ShellItem, Word};

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

pub(crate) fn rewrite_rg_args(args: &[Word]) -> Option<Vec<String>> {
    let mut options = Vec::new();
    let mut positionals = Vec::new();
    let mut has_regexp = false;
    let mut after_double_dash = false;
    let mut i = 0;

    while i < args.len() {
        let arg = args[i].text.as_str();
        if after_double_dash {
            positionals.push(arg.to_string());
            i += 1;
            continue;
        }

        if arg == "--" {
            after_double_dash = true;
            i += 1;
        } else if let Some(next) = parse_rg_long(arg, args, i, &mut options, &mut has_regexp) {
            i = next;
        } else if arg.starts_with('-') && arg != "-" {
            i = parse_rg_short(arg, args, i, &mut options, &mut has_regexp)?;
        } else {
            positionals.push(arg.to_string());
            i += 1;
        }
    }

    if !has_regexp && positionals.is_empty() {
        return None;
    }

    options.extend(positionals);
    Some(options)
}

fn parse_rg_long(
    arg: &str,
    args: &[Word],
    index: usize,
    output: &mut Vec<String>,
    has_regexp: &mut bool,
) -> Option<usize> {
    if !arg.starts_with("--") {
        return None;
    }

    let (name, inline_value) = arg
        .split_once('=')
        .map_or((arg, None), |(name, value)| (name, Some(value.to_string())));

    match name {
        "--line-number" => push_no_value_long(inline_value, output, "-n", index),
        "--fixed-strings" => push_no_value_long(inline_value, output, "-F", index),
        "--ignore-case" => push_no_value_long(inline_value, output, "-i", index),
        "--json" => push_no_value_long(inline_value, output, "--json", index),
        "--type" => push_value_long(inline_value, args, index, output, "-t"),
        "--glob" => push_value_long(inline_value, args, index, output, "-g"),
        "--regexp" => {
            let next = push_value_long(inline_value, args, index, output, "-e")?;
            *has_regexp = true;
            Some(next)
        }
        _ => None,
    }
}

fn push_no_value_long(
    inline_value: Option<String>,
    output: &mut Vec<String>,
    flag: &str,
    index: usize,
) -> Option<usize> {
    if inline_value.is_some() {
        return None;
    }
    output.push(flag.to_string());
    Some(index + 1)
}

fn push_value_long(
    inline_value: Option<String>,
    args: &[Word],
    index: usize,
    output: &mut Vec<String>,
    flag: &str,
) -> Option<usize> {
    output.push(flag.to_string());
    if let Some(value) = inline_value {
        output.push(value);
        return Some(index + 1);
    }
    let value = args.get(index + 1)?;
    output.push(value.text.clone());
    Some(index + 2)
}

fn parse_rg_short(
    arg: &str,
    args: &[Word],
    index: usize,
    output: &mut Vec<String>,
    has_regexp: &mut bool,
) -> Option<usize> {
    let rest = arg.strip_prefix('-')?;
    if rest.is_empty() {
        return None;
    }

    let bytes = rest.as_bytes();
    let mut j = 0;
    while j < bytes.len() {
        let flag = bytes[j] as char;
        match flag {
            'n' => output.push("-n".to_string()),
            'F' => output.push("-F".to_string()),
            'i' => output.push("-i".to_string()),
            't' | 'g' | 'e' => {
                let st_flag = match flag {
                    't' => "-t",
                    'g' => "-g",
                    'e' => "-e",
                    _ => unreachable!(),
                };
                output.push(st_flag.to_string());
                if flag == 'e' {
                    *has_regexp = true;
                }
                let value = if j + 1 < bytes.len() {
                    rest[j + 1..].to_string()
                } else {
                    args.get(index + 1)?.text.clone()
                };
                output.push(value);
                return Some(if j + 1 < bytes.len() {
                    index + 1
                } else {
                    index + 2
                });
            }
            _ => {
                // Intentionally excluded from the rewrite allowlist.
                // These flags have st/rg semantic divergences that would
                // silently corrupt agent results if rewritten:
                //
                //   -v / --invert-match  st -v does a full-corpus scan (every
                //                        file in scope), rg -v is line-level.
                //                        The result sets differ for files with
                //                        no matches at all.
                //   -c / --count         rg -c counts matching lines; st -c
                //                        behavior diverges on multi-match lines.
                //   -U / --multiline     not supported by st; patterns with \n
                //                        simply never match.
                //   --max-depth          repo-root-relative in st (pre-fix),
                //                        search-path-relative in rg.
                //   Any other flag not in the allowlist above is unknown and
                //   must not be silently forwarded. Return None to abort the
                //   rewrite and leave the original `rg` command intact.
                return None;
            }
        }
        j += 1;
    }

    Some(index + 1)
}

fn rewrite_grep_args(args: &[Word]) -> Option<Vec<String>> {
    let mut recursive = false;
    let mut options = Vec::new();
    let mut positionals = Vec::new();
    let mut after_double_dash = false;
    let mut i = 0;

    while i < args.len() {
        let arg = args[i].text.as_str();
        if after_double_dash {
            positionals.push(arg.to_string());
            i += 1;
            continue;
        }

        if arg == "--" {
            after_double_dash = true;
            i += 1;
        } else if let Some(next) = parse_grep_long(arg, i, &mut recursive, &mut options) {
            i = next;
        } else if arg.starts_with('-') && arg != "-" {
            i = parse_grep_short(arg, i, &mut recursive, &mut options)?;
        } else {
            positionals.push(arg.to_string());
            i += 1;
        }
    }

    if !recursive || positionals.len() < 2 {
        return None;
    }

    options.extend(positionals);
    Some(options)
}

fn parse_grep_long(
    arg: &str,
    index: usize,
    recursive: &mut bool,
    output: &mut Vec<String>,
) -> Option<usize> {
    let (name, value) = arg
        .split_once('=')
        .map_or((arg, None), |(name, value)| (name, Some(value)));

    match name {
        "--recursive" | "--dereference-recursive" => {
            if value.is_some() {
                return None;
            }
            *recursive = true;
        }
        "--line-number" => {
            if value.is_some() {
                return None;
            }
            output.push("-n".to_string());
        }
        "--ignore-case" => {
            if value.is_some() {
                return None;
            }
            output.push("-i".to_string());
        }
        "--fixed-strings" => {
            if value.is_some() {
                return None;
            }
            output.push("-F".to_string());
        }
        "--binary-files" if value == Some("without-match") => {}
        _ => return None,
    }
    Some(index + 1)
}

fn parse_grep_short(
    arg: &str,
    index: usize,
    recursive: &mut bool,
    output: &mut Vec<String>,
) -> Option<usize> {
    let rest = arg.strip_prefix('-')?;
    if rest.is_empty() {
        return None;
    }

    for flag in rest.chars() {
        match flag {
            'R' | 'r' => *recursive = true,
            'n' => output.push("-n".to_string()),
            'i' => output.push("-i".to_string()),
            'F' => output.push("-F".to_string()),
            'I' => {}
            _ => return None,
        }
    }
    Some(index + 1)
}

// rewrite_tests.rs is loaded as a sibling module via `mod rewrite_tests;` in
// hook/core/mod.rs (its tests use absolute `crate::hook::core::...` paths), so
// it is NOT re-declared here -- doing so would load the same file as two
// distinct modules (clippy::duplicate_mod).
