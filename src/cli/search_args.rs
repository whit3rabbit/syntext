use std::io::IsTerminal;
use std::path::PathBuf;

use crate::Config;

#[derive(Clone)]
pub(super) struct SearchArgs {
    pub pattern: String,
    pub paths: Vec<PathBuf>,
    pub fixed_strings: bool,
    pub ignore_case: bool,
    pub word_regexp: bool,
    pub line_regexp: bool,
    pub line_number: bool,
    pub with_filename: bool,
    pub invert_match: bool,
    pub files_with_matches: bool,
    pub files_without_match: bool,
    pub count: bool,
    pub count_matches: bool,
    pub max_count: Option<usize>,
    pub quiet: bool,
    pub only_matching: bool,
    pub json: bool,
    pub heading: bool,
    pub no_line_number: bool,
    pub no_filename: bool,
    pub after_context: usize,
    pub before_context: usize,
    pub file_types: Vec<String>,
    pub type_nots: Vec<String>,
    pub globs: Vec<String>,
    pub column: bool,
    pub vimgrep: bool,
    pub replace: Option<String>,
    pub null: bool,
    pub context_separator: String,
    pub byte_offset: bool,
    pub trim: bool,
    pub max_columns: Option<usize>,
    pub search_stats: bool,
    pub max_depth: Option<usize>,
    pub fallback: bool,
    /// Symbol name to look up (`--sym`); routes to the symbol index instead of a
    /// content search. Always `None` when the `symbols` feature is not built.
    #[cfg_attr(not(feature = "symbols"), allow(dead_code))]
    pub sym: Option<String>,
    /// Optional kind filter for `--sym` (raw string, parsed to `SymbolKind`).
    #[cfg_attr(not(feature = "symbols"), allow(dead_code))]
    pub sym_kind: Option<String>,
    /// Find-references target (`--refs`); routes to `Index::search_references`.
    /// Always `None` when the `symbols` feature is not built.
    #[cfg_attr(not(feature = "symbols"), allow(dead_code))]
    pub refs: Option<String>,
    /// Emit ANSI color (resolved from `--color`/`--pretty`/tty in `cli/mod.rs`).
    pub color: bool,
}

impl Default for SearchArgs {
    fn default() -> Self {
        Self {
            pattern: String::new(),
            paths: Vec::new(),
            fixed_strings: false,
            ignore_case: false,
            word_regexp: false,
            line_regexp: false,
            line_number: false,
            with_filename: false,
            invert_match: false,
            files_with_matches: false,
            files_without_match: false,
            count: false,
            count_matches: false,
            max_count: None,
            quiet: false,
            only_matching: false,
            json: false,
            heading: false,
            no_line_number: false,
            no_filename: false,
            after_context: 0,
            before_context: 0,
            file_types: Vec::new(),
            type_nots: Vec::new(),
            globs: Vec::new(),
            column: false,
            vimgrep: false,
            replace: None,
            null: false,
            context_separator: "--".to_string(),
            byte_offset: false,
            trim: false,
            max_columns: None,
            search_stats: false,
            max_depth: None,
            fallback: false,
            sym: None,
            sym_kind: None,
            refs: None,
            color: false,
        }
    }
}

impl SearchArgs {
    pub(super) fn with_effective_output_defaults(&self, config: &Config) -> Self {
        let mut effective = self.clone();
        let stdout_is_tty = std::io::stdout().is_terminal();

        if !self.line_number && !self.no_line_number {
            effective.no_line_number = !stdout_is_tty;
        }

        if self.with_filename {
            effective.no_filename = false;
        } else if !self.no_filename {
            effective.no_filename = !super::scope::shows_filename_by_default(config, &self.paths);
        }

        effective
    }
}
