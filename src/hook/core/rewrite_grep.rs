//! Grep argument rewriting rules.

use super::shell::Word;

pub(crate) fn rewrite_grep_args(args: &[Word]) -> Option<Vec<String>> {
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
        } else if let Some(next) = parse_grep_long(arg, args, i, &mut recursive, &mut options) {
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

    // Re-emit `--` before positionals so the pattern (grep's first positional)
    // reaches st as a value, not a flag bundle: `grep -rn -- --foo src` ->
    // `st -n -- --foo src`. grep only rewrites with >= 2 positionals (guard
    // above), so the separator is always meaningful here.
    options.push("--".to_string());
    options.extend(positionals);
    Some(options)
}

fn parse_grep_long(
    arg: &str,
    args: &[Word],
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
        "--binary-files" => {
            // Only `without-match` is a safe no-op; `text`/`binary` change match
            // semantics st does not replicate, so abort (stay on grep). Accept
            // both inline (`=without-match`) and space-separated (`--binary-files
            // without-match`) forms; the latter consumes the next token.
            match value {
                Some("without-match") => {}
                Some(_) => return None,
                None => {
                    if args.get(index + 1).map(|w| w.text.as_str()) != Some("without-match") {
                        return None;
                    }
                    return Some(index + 2);
                }
            }
        }
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
