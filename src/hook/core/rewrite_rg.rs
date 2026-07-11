//! Ripgrep argument rewriting rules.

use super::shell::Word;

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

    // Re-emit a `--` before the positionals so st's clap parser treats them as
    // trailing values, not flags or subcommands. Without it a bare pattern equal
    // to a subcommand name (`rg status .` -> `st status .`) routes to that
    // subcommand, and a leading-dash pattern (`rg -- -foo`) parses as a flag
    // bundle. When positionals is empty the pattern came via `-e` (already in
    // `options`), so no separator is needed.
    if !positionals.is_empty() {
        options.push("--".to_string());
        options.extend(positionals);
    }
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
                    // Unreachable given the outer `'t' | 'g' | 'e'` arm, but this
                    // hook rewrites *agent* commands; a future allowlist drift
                    // would panic here and kill the agent's tool call. Aborting
                    // the rewrite (return None) is strictly safer and the cost
                    // is just leaving the original `rg` command intact.
                    _ => return None,
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
