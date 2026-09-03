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
}

impl SearchOptionsJson {
    /// Convert to [`SearchOptions`], applying the FFI `max_results` policy.
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
        }
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
