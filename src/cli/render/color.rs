//! Color output: SGR style emission and the `always`/`never`/`auto` decision.
//!
//! v1 ships a fixed ripgrep-style default palette (match = bold red, path =
//! magenta, line = green) and `--color {always,never,auto,ansi}`. The
//! `--colors SPEC` grammar is accepted but not yet honoured (deferred); this
//! module owns the styles so a future SPEC parser has one place to plug into.
//!
//! No ANSI crate is pulled in: three fixed styles plus a reset is ~20 lines,
//! and tty detection reuses `std::io::IsTerminal` (already used in
//! `cli/search.rs`), so there is no new dependency.

use std::io::{self, IsTerminal, Write};

/// Reset all SGR attributes.
pub(in crate::cli) const RESET: &[u8] = b"\x1b[0m";

/// Fixed v1 palette (ripgrep defaults). `--colors` customisation is deferred.
#[derive(Clone, Copy)]
pub(in crate::cli) struct ColorStyles {
    /// Matched text: bold red.
    pub match_: &'static [u8],
    /// File path: magenta.
    pub path: &'static [u8],
    /// Line number: green.
    pub line: &'static [u8],
}

impl Default for ColorStyles {
    fn default() -> Self {
        Self {
            match_: b"\x1b[1;31m",
            path: b"\x1b[35m",
            line: b"\x1b[32m",
        }
    }
}

/// `--color WHEN` value.
pub(in crate::cli) enum ColorWhen {
    Always,
    Never,
    Auto,
    Ansi,
}

impl ColorWhen {
    /// Parse `--color`'s argument; `None` means the flag was absent. clap's
    /// `value_parser` restricts input to the four known values, so the
    /// `Some(_)` fallthrough only fires for programmatic construction and
    /// behaves like `auto` (tty-gated) rather than silently forcing color.
    pub(in crate::cli) fn parse(s: Option<&str>) -> Option<ColorWhen> {
        match s {
            None => None,
            Some("always") => Some(ColorWhen::Always),
            Some("never") => Some(ColorWhen::Never),
            Some("auto") => Some(ColorWhen::Auto),
            Some("ansi") => Some(ColorWhen::Ansi),
            Some(_) => Some(ColorWhen::Auto),
        }
    }
}

/// Resolve whether to emit ANSI color.
///
/// `always`/`ansi` force on; `never` forces off; an absent flag or `auto`
/// colors only when stdout is a TTY. `--pretty` (which documents
/// `--color=always`) forces color on unless the user explicitly passed
/// `never`, so `--pretty --color=never` stays monochrome.
///
/// `NO_COLOR` (https://no-color.org, honored by rg/grep/ls/etc.) forces color
/// off when set to any non-empty value — but only under `auto`/absent, since an
/// explicit `--color=always`/`ansi` is the user overriding it. This keeps the
/// accessibility convention (no surprise color in piped/piped-from-no-color
/// environments) without breaking `--color=always` scripts.
pub(in crate::cli) fn resolve_color(when: Option<ColorWhen>, pretty: bool) -> bool {
    match when {
        Some(ColorWhen::Always) | Some(ColorWhen::Ansi) => true,
        Some(ColorWhen::Never) => false,
        None | Some(ColorWhen::Auto) => {
            if no_color_set() {
                return false;
            }
            pretty || io::stdout().is_terminal()
        }
    }
}

/// True when the `NO_COLOR` env var is present and non-empty (per the
/// no-color.org spec, which keys on presence+non-emptiness, not the value).
fn no_color_set() -> bool {
    std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty())
}

/// Write `bytes` wrapped in `style`...`RESET` when `color` is on, raw
/// otherwise. The `color` flag lets callers swap a raw `write_all` for this
/// without an extra branch.
pub(in crate::cli) fn write_styled(
    out: &mut dyn Write,
    color: bool,
    style: &[u8],
    bytes: &[u8],
) -> io::Result<()> {
    if color {
        out.write_all(style)?;
        out.write_all(bytes)?;
        out.write_all(RESET)
    } else {
        out.write_all(bytes)
    }
}

/// Write a decimal `usize` wrapped in `style`...`RESET` when `color` is on.
pub(in crate::cli) fn write_styled_num(
    out: &mut dyn Write,
    color: bool,
    style: &[u8],
    n: usize,
) -> io::Result<()> {
    if color {
        out.write_all(style)?;
        write!(out, "{n}")?;
        out.write_all(RESET)
    } else {
        write!(out, "{n}")
    }
}

/// Write `content`, wrapping each byte span in `styles.match_`...`RESET` when
/// `color` is on. Spans must be sorted, non-overlapping byte ranges into
/// `content` (as `Regex::find_iter` yields); overlapping/out-of-range spans
/// are clamped or skipped so a bad range can never panic. When color is off,
/// `content` is written verbatim.
pub(in crate::cli) fn write_highlighted(
    out: &mut dyn Write,
    color: bool,
    styles: ColorStyles,
    content: &[u8],
    spans: &[(usize, usize)],
) -> io::Result<()> {
    if !color {
        return out.write_all(content);
    }
    let mut cursor = 0usize;
    for &(start, end) in spans {
        // Skip non-advancing or out-of-order spans (defensive; find_iter is
        // well-behaved, but match_spans callers could in principle hand junk).
        if start < cursor {
            continue;
        }
        let start = start.min(content.len());
        let end = end.min(content.len());
        if start >= end {
            continue;
        }
        if start > cursor {
            out.write_all(&content[cursor..start])?;
        }
        out.write_all(styles.match_)?;
        out.write_all(&content[start..end])?;
        out.write_all(RESET)?;
        cursor = end;
    }
    if cursor < content.len() {
        out.write_all(&content[cursor..])?;
    }
    Ok(())
}
