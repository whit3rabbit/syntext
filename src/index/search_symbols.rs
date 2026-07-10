#![cfg(feature = "symbols")]

use crate::symbol::extractor::SymbolKind;
use crate::{IndexError, SearchMatch, SearchOptions};

impl super::Index {
    /// Look up symbols by name (prefix match) in the symbol index.
    ///
    /// `kind` is an optional symbol-kind filter (e.g. `"function"`, `"struct"`);
    /// an unrecognized kind returns [`IndexError::InvalidPattern`]. Returns an
    /// empty result when the symbol index was never built.
    pub fn search_symbols(
        &self,
        name: &str,
        kind: Option<&str>,
    ) -> Result<Vec<SearchMatch>, IndexError> {
        let kind_filter = match kind {
            Some(k) => Some(
                k.parse::<SymbolKind>()
                    .map_err(|_| IndexError::InvalidPattern(format!("unknown symbol kind: {k}")))?,
            ),
            None => None,
        };
        match &self.symbol_index {
            Some(sym_idx) => sym_idx.search(name, kind_filter),
            None => Ok(Vec::new()),
        }
    }

    /// Find references to a symbol: resolve `name` to its definition name(s) via
    /// the symbol index, then run a word-boundary, case-sensitive content search
    /// for each resolved identifier.
    ///
    /// Returns content matches (real `line_content`, `byte_offset`, and
    /// `submatch` spans), not symbol lookups, so they render through the normal
    /// grep-style pipeline. Not scope-aware: a bare identifier in an unrelated
    /// scope, a string literal, or a comment all match. Returns empty when the
    /// symbol index was never built or `name` matches no definition exactly.
    pub fn search_references(
        &self,
        name: &str,
        kind: Option<&str>,
    ) -> Result<Vec<SearchMatch>, IndexError> {
        // search_symbols is prefix-LIKE; keep only definitions whose name matches
        // `name` exactly (case-insensitively) so `--refs parse` does not silently
        // target `parse_query`.
        let defs = self.search_symbols(name, kind)?;
        let target = name.to_lowercase();
        let names: std::collections::BTreeSet<String> = defs
            .into_iter()
            .map(|d| String::from_utf8_lossy(&d.line_content).into_owned())
            .filter(|n| n.to_lowercase() == target)
            .collect();
        if names.is_empty() {
            return Ok(Vec::new());
        }
        // For each resolved name, run the equivalent of `st -w -s <name>`:
        // route on the escaped literal (so a name with regex metacharacters is
        // not misread by the gram router) and verify with a word-boundary regex.
        // The tokenizer lowercased at index time for candidate selection, but the
        // verifier re-checks the original bytes, so the match is case-sensitive.
        let mut all = Vec::new();
        for full_name in &names {
            let escaped = regex::escape(full_name);
            let opts = SearchOptions {
                verify_pattern: Some(format!(r"\b(?:{escaped})\b")),
                case_insensitive: false,
                ..Default::default()
            };
            all.extend(self.search(&escaped, &opts)?);
        }
        // One SearchMatch per matched line per name; dedup lines hit by more than
        // one resolved name (e.g. two overloaded defs sharing an identifier).
        all.sort_unstable_by(|a, b| {
            a.path
                .cmp(&b.path)
                .then_with(|| a.line_number.cmp(&b.line_number))
        });
        all.dedup_by(|a, b| a.path == b.path && a.line_number == b.line_number);
        Ok(all)
    }
}
