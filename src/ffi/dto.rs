//! JSON DTOs crossing the FFI boundary.
//!
//! Input DTOs are tolerant: `#[serde(default)]`, snake_case, unknown fields
//! ignored — so adding Rust-side fields later is forward-compatible for
//! already-compiled callers. Output DTOs mirror the plan's published schemas
//! (docs/SWIFT.md is the reference).

use std::path::PathBuf;

use crate::index::freshness::{UpdateLimits, UpdateOutcome};
use crate::{Config, IndexStats, SearchMatch, SearchOptions};

use super::{DEFAULT_MAX_RESULTS, MAX_MAX_RESULTS};

// ── Inputs ──────────────────────────────────────────────────
/// `SearchOptions` as JSON. Applied on top of [`SearchOptions::default`].
#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
pub(crate) struct SearchOptionsJson {
    /// Glob pattern restricting search to matching paths.
    pub path_filter: Option<String>,
    /// Single file-type filter (e.g. `"rs"`); unioned with `file_types`.
    pub file_type: Option<String>,
    /// Single excluded file type; combined with `exclude_types`.
    pub exclude_type: Option<String>,
    /// File types to include (unioned via the path index).
    pub file_types: Vec<String>,
    /// File types to exclude.
    pub exclude_types: Vec<String>,
    /// Max results. FFI layer defaults absent/0 to 10_000 and clamps to
    /// 1_000_000 (see `DEFAULT_MAX_RESULTS` / `MAX_MAX_RESULTS`).
    pub max_results: Option<usize>,
    /// Case-insensitive matching.
    pub case_insensitive: bool,
    /// Alternative verification pattern (boundary wrapping).
    pub verify_pattern: Option<String>,
    /// Leave `line_content` empty (count-free file-list style).
    pub skip_line_content: bool,
    /// Force deterministic ordering (slower on large result sets).
    pub deterministic: bool,
    /// Treat the pattern as a literal string instead of a regular expression.
    pub fixed_strings: bool,
    /// Only show matches surrounded by word boundaries (like `rg -w`).
    pub word_regexp: bool,
    /// Only show matches surrounded by line boundaries (like `rg -x`).
    pub line_regexp: bool,
    /// Number of context lines to capture before each match (`-B`).
    pub before_context: Option<usize>,
    /// Number of context lines to capture after each match (`-A`).
    pub after_context: Option<usize>,
}

impl SearchOptionsJson {
    /// Convert to [`SearchOptions`], applying the FFI `max_results` policy.
    #[allow(dead_code)]
    pub(crate) fn into_search_options(self) -> SearchOptions {
        let max_results = match self.max_results {
            // 0 would mean "no cap"; clamp to the default instead so a
            // malformed caller cannot request unbounded materialization.
            None | Some(0) => Some(DEFAULT_MAX_RESULTS),
            Some(n) => Some(n.min(MAX_MAX_RESULTS)),
        };
        SearchOptions {
            path_filter: self.path_filter,
            file_type: self.file_type,
            exclude_type: self.exclude_type,
            file_types: self.file_types,
            exclude_types: self.exclude_types,
            max_results,
            case_insensitive: self.case_insensitive,
            verify_pattern: self.verify_pattern,
            skip_line_content: self.skip_line_content,
            deterministic: self.deterministic,
            ..SearchOptions::default()
        }
    }

    /// Convert to [`SearchOptions`] and apply effective pattern transformations
    /// (e.g. `fixed_strings`, `word_regexp`, `line_regexp`).
    pub(crate) fn into_search_options_and_effective_pattern(
        self,
        pattern: &str,
    ) -> (String, SearchOptions) {
        let (routing_pattern, verify_pattern) = build_effective_pattern(
            pattern,
            self.fixed_strings,
            self.word_regexp,
            self.line_regexp,
            self.verify_pattern,
        );
        let max_results = match self.max_results {
            None | Some(0) => Some(DEFAULT_MAX_RESULTS),
            Some(n) => Some(n.min(MAX_MAX_RESULTS)),
        };
        let opts = SearchOptions {
            path_filter: self.path_filter,
            file_type: self.file_type,
            exclude_type: self.exclude_type,
            file_types: self.file_types,
            exclude_types: self.exclude_types,
            max_results,
            case_insensitive: self.case_insensitive,
            verify_pattern,
            skip_line_content: self.skip_line_content,
            deterministic: self.deterministic,
            ..SearchOptions::default()
        };
        (routing_pattern, opts)
    }
}

/// Subset of [`Config`] exposed over the FFI. Paths come from the C
/// arguments, not JSON. Applied on top of [`Config::new`] defaults.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
pub(crate) struct ConfigJson {
    /// Maximum file size to index (bytes).
    pub max_file_size: Option<u64>,
    /// Maximum segments before triggering a merge.
    pub max_segments: Option<usize>,
    /// Reject index dirs with group/other permission bits (unix).
    pub strict_permissions: Option<bool>,
    /// Fully checksum each segment at open time (O(postings) I/O).
    pub verify_on_open: Option<bool>,
}

impl ConfigJson {
    /// Build a [`Config`] for `index_dir`/`repo_root`, overriding defaults
    /// only where the JSON set a field.
    pub(crate) fn into_config(self, index_dir: PathBuf, repo_root: PathBuf) -> Config {
        let mut config = Config::new(index_dir, repo_root);
        if let Some(v) = self.max_file_size {
            config.max_file_size = v;
        }
        if let Some(v) = self.max_segments {
            config.max_segments = v;
        }
        if let Some(v) = self.strict_permissions {
            config.strict_permissions = v;
        }
        if let Some(v) = self.verify_on_open {
            config.verify_on_open = v;
        }
        config
    }
}

/// [`UpdateLimits`] as JSON. NULL JSON means the CLI defaults (200 files,
/// 150 ms); explicit `null` fields mean "no limit" for that bound.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
pub(crate) struct LimitsJson {
    /// Max changed files to process (None = no limit).
    pub max_files: Option<usize>,
    /// Elapsed-time budget in ms for git detection (None = no limit).
    pub budget_ms: Option<u64>,
}

impl LimitsJson {
    pub(crate) fn into_limits(self) -> UpdateLimits {
        UpdateLimits {
            max_files: self.max_files,
            budget_ms: self.budget_ms,
        }
    }
}

// ── Outputs ─────────────────────────────────────────────────
/// One context line surrounding a match.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub(crate) struct ContextLineDto {
    /// 1-based line number.
    pub line_number: u32,
    /// Rendered line content (display string).
    pub line_content: String,
    /// Whether this line is the matching line itself.
    pub is_match: bool,
}

/// One search match. `line_content` is a lossy UTF-8 rendering for display;
/// `line_content_b64` is the exact bytes. `submatch_start`, `submatch_end`,
/// and `byte_offset` are defined ONLY against the base64-decoded bytes.
#[derive(Debug, serde::Serialize)]
pub(crate) struct MatchDto {
    /// Repo-relative path or chat document id.
    pub path: String,
    /// 1-based line number.
    pub line_number: u32,
    /// Lossy UTF-8 rendering of the matched line (display only).
    pub line_content: String,
    /// Standard base64 (RFC 4648, padded) of the exact line bytes.
    pub line_content_b64: String,
    /// Byte offset of the first match within the document.
    pub byte_offset: u64,
    /// Byte offset of the match start within the decoded line bytes.
    pub submatch_start: u64,
    /// Exclusive byte offset of the match end within the decoded line bytes.
    pub submatch_end: u64,
    /// Context lines surrounding this match (empty if context was not requested).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context: Vec<ContextLineDto>,
}

impl MatchDto {
    pub(crate) fn from_search_match(m: &SearchMatch) -> Self {
        MatchDto {
            path: m.path.to_string_lossy().into_owned(),
            line_number: m.line_number,
            line_content: String::from_utf8_lossy(&m.line_content).into_owned(),
            line_content_b64: crate::base64::encode(&m.line_content),
            byte_offset: m.byte_offset,
            submatch_start: m.submatch_start as u64,
            submatch_end: m.submatch_end as u64,
            context: Vec::new(),
        }
    }
}

/// Convert a list of [`FileMatches`](crate::FileMatches) into [`MatchDto`] items,
/// extracting up to `before` and `after` context lines for each match.
pub(crate) fn from_file_matches(
    file_matches: &[crate::FileMatches],
    before: usize,
    after: usize,
) -> Vec<MatchDto> {
    let mut dtos = Vec::new();
    for fm in file_matches {
        for m in &fm.matches {
            let mut dto = MatchDto::from_search_match(m);
            if before > 0 || after > 0 {
                let ctx = fm.context(m.line_number, before, after);
                dto.context = ctx
                    .into_iter()
                    .map(|(n, line_bytes, is_match)| ContextLineDto {
                        line_number: n,
                        line_content: String::from_utf8_lossy(line_bytes).into_owned(),
                        is_match,
                    })
                    .collect();
            }
            dtos.push(dto);
        }
    }
    dtos
}

// ── Pattern wrapping helpers (ripgrep -F, -w, -x) ───────────
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn split_top_level_alternatives(pat: &str) -> Vec<String> {
    let mut alts = Vec::new();
    let mut current = String::new();
    let mut depth = 0;
    let mut in_bracket = false;
    let mut chars = pat.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\\' {
            current.push(c);
            if let Some(next_c) = chars.next() {
                current.push(next_c);
            }
            continue;
        }
        if c == '[' && !in_bracket {
            in_bracket = true;
            current.push(c);
            continue;
        }
        if c == ']' && in_bracket {
            in_bracket = false;
            current.push(c);
            continue;
        }
        if in_bracket {
            current.push(c);
            continue;
        }
        if c == '(' {
            depth += 1;
            current.push(c);
            continue;
        }
        if c == ')' {
            if depth > 0 {
                depth -= 1;
            }
            current.push(c);
            continue;
        }
        if c == '|' && depth == 0 {
            alts.push(current);
            current = String::new();
            continue;
        }
        current.push(c);
    }
    alts.push(current);
    alts
}

fn hir_starts_with_word_char(hir: &regex_syntax::hir::Hir) -> bool {
    use regex_syntax::hir::{Class, HirKind};
    match hir.kind() {
        HirKind::Empty | HirKind::Look(_) => false,
        HirKind::Literal(lit) => {
            if lit.0.is_empty() {
                false
            } else {
                let first_byte = lit.0[0] as char;
                is_word_char(first_byte)
            }
        }
        HirKind::Concat(subs) => subs.first().is_some_and(hir_starts_with_word_char),
        HirKind::Alternation(subs) => subs.iter().any(hir_starts_with_word_char),
        HirKind::Repetition(rep) => hir_starts_with_word_char(&rep.sub),
        HirKind::Capture(cap) => hir_starts_with_word_char(&cap.sub),
        HirKind::Class(class) => match class {
            Class::Unicode(u) => u.ranges().iter().any(|r| {
                (r.start() as u32..=r.end() as u32)
                    .any(|cp| std::char::from_u32(cp).is_some_and(is_word_char))
            }),
            Class::Bytes(b) => b
                .ranges()
                .iter()
                .any(|r| (r.start()..=r.end()).any(|byte| is_word_char(byte as char))),
        },
    }
}

fn hir_ends_with_word_char(hir: &regex_syntax::hir::Hir) -> bool {
    use regex_syntax::hir::{Class, HirKind};
    match hir.kind() {
        HirKind::Empty | HirKind::Look(_) => false,
        HirKind::Literal(lit) => {
            if lit.0.is_empty() {
                false
            } else {
                let last_byte = lit.0[lit.0.len() - 1] as char;
                is_word_char(last_byte)
            }
        }
        HirKind::Concat(subs) => subs.last().is_some_and(hir_ends_with_word_char),
        HirKind::Alternation(subs) => subs.iter().any(hir_ends_with_word_char),
        HirKind::Repetition(rep) => hir_ends_with_word_char(&rep.sub),
        HirKind::Capture(cap) => hir_ends_with_word_char(&cap.sub),
        HirKind::Class(class) => match class {
            Class::Unicode(u) => u.ranges().iter().any(|r| {
                (r.start() as u32..=r.end() as u32)
                    .any(|cp| std::char::from_u32(cp).is_some_and(is_word_char))
            }),
            Class::Bytes(b) => b
                .ranges()
                .iter()
                .any(|r| (r.start()..=r.end()).any(|byte| is_word_char(byte as char))),
        },
    }
}

fn get_boundary_chars(alt: &str) -> (Option<char>, Option<char>) {
    if let Ok(hir) = regex_syntax::ParserBuilder::new()
        .utf8(false)
        .build()
        .parse(alt)
    {
        let starts_with_word = hir_starts_with_word_char(&hir);
        let ends_with_word = hir_ends_with_word_char(&hir);
        let start_c = if starts_with_word {
            Some('a')
        } else {
            Some(';')
        };
        let end_c = if ends_with_word { Some('a') } else { Some(';') };
        return (start_c, end_c);
    }

    let mut inner = alt;
    if inner.starts_with("(?:") && inner.ends_with(')') {
        inner = &inner[3..inner.len() - 1];
    }
    (inner.chars().next(), inner.chars().next_back())
}

pub(crate) fn build_effective_pattern(
    pattern: &str,
    fixed_strings: bool,
    word_regexp: bool,
    line_regexp: bool,
    verify_pattern: Option<String>,
) -> (String, Option<String>) {
    let pat = if fixed_strings {
        regex::escape(pattern)
    } else {
        pattern.to_string()
    };
    if line_regexp {
        let wrapped = format!("^(?:{pat})$");
        (pat, Some(wrapped))
    } else if word_regexp {
        let alts = split_top_level_alternatives(&pat);

        let mut all_start_word = true;
        let mut all_start_non_word = true;
        let mut all_end_word = true;
        let mut all_end_non_word = true;

        let mut alt_bounds = Vec::new();
        for alt in &alts {
            let (start_c, end_c) = get_boundary_chars(alt);
            let start_word = start_c.is_some_and(is_word_char);
            let end_word = end_c.is_some_and(is_word_char);
            alt_bounds.push((start_word, end_word));

            if start_word {
                all_start_non_word = false;
            } else {
                all_start_word = false;
            }
            if end_word {
                all_end_non_word = false;
            } else {
                all_end_word = false;
            }
        }

        let wrapped =
            if (all_start_word || all_start_non_word) && (all_end_word || all_end_non_word) {
                let start_bound = if all_start_word { r"\b" } else { r"\B" };
                let end_bound = if all_end_word { r"\b" } else { r"\B" };
                format!(r"{start_bound}(?:{pat}){end_bound}")
            } else {
                let wrapped_alts: Vec<String> = alts
                    .iter()
                    .zip(alt_bounds.iter())
                    .map(|(alt, &(start_word, end_word))| {
                        let start_bound = if start_word { r"\b" } else { r"\B" };
                        let end_bound = if end_word { r"\b" } else { r"\B" };
                        format!(r"{start_bound}(?:{alt}){end_bound}")
                    })
                    .collect();
                wrapped_alts.join("|")
            };

        (pat, Some(wrapped))
    } else {
        (pat, verify_pattern)
    }
}

/// Mirror of [`IndexStats`] (usize widened to u64 for stable JSON).
#[derive(Debug, serde::Serialize)]
pub(crate) struct StatsDto {
    pub total_documents: u64,
    pub total_segments: u64,
    pub total_grams: u64,
    pub index_size_bytes: u64,
    pub base_commit: Option<String>,
    pub overlay_generations: u64,
    pub pending_edits: u64,
}

impl From<IndexStats> for StatsDto {
    fn from(s: IndexStats) -> Self {
        StatsDto {
            total_documents: s.total_documents as u64,
            total_segments: s.total_segments as u64,
            total_grams: s.total_grams as u64,
            index_size_bytes: s.index_size_bytes,
            base_commit: s.base_commit,
            overlay_generations: s.overlay_generations as u64,
            pending_edits: s.pending_edits as u64,
        }
    }
}

/// Mirror of [`UpdateOutcome`], tagged with `"kind"` (snake_case).
#[derive(Debug, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum UpdateOutcomeDto {
    /// Applied `files` change notifications; `skipped` counts files left stale.
    Updated {
        files: u64,
        skipped: u64,
        detect_elapsed_ms: u64,
    },
    /// No changes detected since the last build.
    NoChanges { detect_elapsed_ms: u64 },
    /// Time budget exhausted; index not updated.
    BudgetExceeded {
        files_behind_estimate: u64,
        detect_elapsed_ms: u64,
    },
    /// Change set exceeded `max_files`; index not updated.
    TooManyFiles {
        files_behind: u64,
        detect_elapsed_ms: u64,
    },
    /// Applying changes would exceed the overlay cap; index not updated.
    OverlayFull {
        files_behind: u64,
        detect_elapsed_ms: u64,
    },
}

impl From<UpdateOutcome> for UpdateOutcomeDto {
    fn from(o: UpdateOutcome) -> Self {
        match o {
            UpdateOutcome::Updated {
                files,
                skipped,
                detect_elapsed_ms,
            } => UpdateOutcomeDto::Updated {
                files: files as u64,
                skipped: skipped as u64,
                detect_elapsed_ms,
            },
            UpdateOutcome::NoChanges { detect_elapsed_ms } => {
                UpdateOutcomeDto::NoChanges { detect_elapsed_ms }
            }
            UpdateOutcome::BudgetExceeded {
                files_behind_estimate,
                detect_elapsed_ms,
            } => UpdateOutcomeDto::BudgetExceeded {
                files_behind_estimate: files_behind_estimate as u64,
                detect_elapsed_ms,
            },
            UpdateOutcome::TooManyFiles {
                files_behind,
                detect_elapsed_ms,
            } => UpdateOutcomeDto::TooManyFiles {
                files_behind: files_behind as u64,
                detect_elapsed_ms,
            },
            UpdateOutcome::OverlayFull {
                files_behind,
                detect_elapsed_ms,
            } => UpdateOutcomeDto::OverlayFull {
                files_behind: files_behind as u64,
                detect_elapsed_ms,
            },
        }
    }
}

/// Envelope returned by `syntext_index_search_fresh`.
#[derive(Debug, serde::Serialize)]
pub(crate) struct SearchFreshDto {
    pub matches: Vec<MatchDto>,
    pub update_outcome: UpdateOutcomeDto,
}
