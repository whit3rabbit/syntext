//! Only-matching output renderer: prints only the matched portions of lines.

use std::collections::BTreeSet;
use std::io::{self, Write};
use std::path::Path;

use crate::path_util::path_bytes;
use crate::search::lines::for_each_line;
use crate::Config;

use super::{
    compile_output_regex, group_matches_by_path, read_matched_file, repo_canonical_root,
    write_formatted_line, ColorStyles,
};
use super::color::write_styled;
use crate::cli::search::SearchArgs;

pub(in crate::cli) fn render_only_matching(
    config: &Config,
    matches: &[crate::SearchMatch],
    args: &SearchArgs,
) -> io::Result<()> {
    let re = compile_output_regex(args)?;
    if args.before_context > 0 || args.after_context > 0 {
        return render_only_matching_with_context(config, matches, args, &re);
    }
    let styles = ColorStyles::default();
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let unique_paths: BTreeSet<_> = matches.iter().map(|m| m.path.clone()).collect();
    let grouped_heading = args.heading && (unique_paths.len() > 1 || !args.no_filename);
    let suppress_path_prefix = args.heading;
    let mut current_path: Option<&Path> = None;

    for m in matches {
        let path_changed = current_path != Some(m.path.as_path());
        if path_changed && grouped_heading {
            if current_path.is_some() {
                writeln!(out)?;
            }
            if !args.no_filename {
                write_styled(&mut out, args.color, styles.path, path_bytes(&m.path).as_ref())?;
                if args.null {
                    out.write_all(b"\0")?;
                } else {
                    out.write_all(b"\n")?;
                }
            }
        }
        current_path = Some(m.path.as_path());

        // In -o mode, rg prints the byte offset of each matched substring, not
        // the line start. line_start = first-submatch abs offset minus its column.
        let line_start = m.byte_offset.saturating_sub(m.submatch_start as u64);
        for matched in re.find_iter(&m.line_content) {
            if matched.start() == matched.end() {
                continue;
            }
            if args.byte_offset {
                write!(out, "{}:", line_start + matched.start() as u64)?;
            }
            let matched_bytes = &m.line_content[matched.start()..matched.end()];
            let rendered = apply_match_replace(&re, args.replace.as_deref(), matched_bytes);
            // -o prints only the match, so the whole rendered slice is the span.
            let spans: Vec<(usize, usize)> = if args.color {
                vec![(0, rendered.as_ref().len())]
            } else {
                Vec::new()
            };
            write_formatted_line(
                &mut out,
                super::FormatOpts {
                    no_path: suppress_path_prefix || args.no_filename,
                    no_num: args.no_line_number,
                    null: args.null,
                    color: args.color,
                },
                &m.path,
                m.line_number as usize,
                b':',
                rendered.as_ref(),
                &spans,
            )?;
        }
    }

    Ok(())
}

/// Apply `--replace` to a single matched substring, expanding capture refs.
/// The slice is exactly one match, so `replace` (first-match) expands `$1` etc.
/// against that match's captures. Returns the raw slice when no replacement.
fn apply_match_replace<'a>(
    re: &regex::bytes::Regex,
    replacement: Option<&str>,
    matched: &'a [u8],
) -> std::borrow::Cow<'a, [u8]> {
    match replacement {
        Some(repl) => re.replace(matched, repl.as_bytes()),
        None => std::borrow::Cow::Borrowed(matched),
    }
}

fn render_only_matching_with_context(
    config: &Config,
    matches: &[crate::SearchMatch],
    args: &SearchArgs,
    re: &regex::bytes::Regex,
) -> io::Result<()> {
    let by_file = group_matches_by_path(matches);

    let stdout = io::stdout();
    let mut out = stdout.lock();
    let styles = ColorStyles::default();
    let before = args.before_context;
    let after = args.after_context;
    let mut first_file = true;
    let grouped_heading = args.heading && (by_file.len() > 1 || !args.no_filename);
    let suppress_path_prefix = args.heading;
    let canonical_root = repo_canonical_root(config);

    for (rel_path, match_lines) in &by_file {
        let Some(raw_content) = read_matched_file(config, &canonical_root, rel_path, args.quiet)
        else {
            continue;
        };
        let file_content = crate::index::normalize_encoding(&raw_content, config.verbose);
        // Keep the line-start byte offset for -b (see render_only_matching).
        let mut file_lines: Vec<(usize, Vec<u8>)> = Vec::new();
        for_each_line(file_content.as_ref(), |_, line_start, line| {
            file_lines.push((line_start, line.to_vec()))
        });

        let match_set: BTreeSet<usize> = match_lines
            .iter()
            .map(|&n| (n as usize).saturating_sub(1))
            .collect();
        let mut to_print: BTreeSet<usize> = BTreeSet::new();
        for &mi in &match_set {
            let start = mi.saturating_sub(before);
            let end = (mi + after).min(file_lines.len().saturating_sub(1));
            for idx in start..=end {
                to_print.insert(idx);
            }
        }

        if !first_file && !to_print.is_empty() {
            if grouped_heading {
                writeln!(out)?;
            } else {
                writeln!(out, "{}", args.context_separator)?;
            }
        }
        if grouped_heading && !to_print.is_empty() && !args.no_filename {
            write_styled(&mut out, args.color, styles.path, path_bytes(rel_path).as_ref())?;
            if args.null {
                out.write_all(b"\0")?;
            } else {
                out.write_all(b"\n")?;
            }
        }
        first_file = false;

        let mut prev: Option<usize> = None;
        for &idx in &to_print {
            if let Some(p) = prev {
                if idx > p + 1 {
                    writeln!(out, "{}", args.context_separator)?;
                }
            }

            let line_num = idx + 1;
            let (line_start, content) = file_lines
                .get(idx)
                .map(|(s, l)| (*s, l.as_slice()))
                .unwrap_or((0, &[][..]));
            if match_set.contains(&idx) {
                for matched in re.find_iter(content) {
                    if matched.start() == matched.end() {
                        continue;
                    }
                    if args.byte_offset {
                        write!(out, "{}:", (line_start + matched.start()) as u64)?;
                    }
                    let matched_bytes = &content[matched.start()..matched.end()];
                    let rendered = apply_match_replace(re, args.replace.as_deref(), matched_bytes);
                    let spans: Vec<(usize, usize)> = if args.color {
                        vec![(0, rendered.as_ref().len())]
                    } else {
                        Vec::new()
                    };
                    write_formatted_line(
                        &mut out,
                        super::FormatOpts {
                            no_path: suppress_path_prefix || args.no_filename,
                            no_num: args.no_line_number,
                            null: args.null,
                            color: args.color,
                        },
                        rel_path,
                        line_num,
                        b':',
                        rendered.as_ref(),
                        &spans,
                    )?;
                }
            } else {
                if args.byte_offset {
                    write!(out, "{line_start}:")?;
                }
                write_formatted_line(
                    &mut out,
                    super::FormatOpts {
                        no_path: suppress_path_prefix || args.no_filename,
                        no_num: args.no_line_number,
                        null: args.null,
                        color: args.color,
                    },
                    rel_path,
                    line_num,
                    b'-',
                    content,
                    &[],
                )?;
            }

            prev = Some(idx);
        }
    }

    Ok(())
}
