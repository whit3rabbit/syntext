//! Symbol index: Tree-sitter extraction + SQLite storage and lookup.
//!
//! Enabled only with `--features symbols`. Build populates the DB;
//! `search()` queries it and returns `SearchMatch` results.

pub mod extractor;

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{params, Connection};

use crate::{IndexError, SearchMatch};

use extractor::{extract_symbols, SymbolKind};

/// SQLite-backed symbol index.
pub struct SymbolIndex {
    conn: Mutex<Connection>,
}

impl SymbolIndex {
    /// Open or create a symbol index at `db_path`.
    pub fn open(db_path: &Path) -> Result<Self, IndexError> {
        let conn = Connection::open(db_path)
            .map_err(|e| IndexError::CorruptIndex(format!("symbol db: {e}")))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS symbols (
                 id      INTEGER PRIMARY KEY,
                 name    TEXT NOT NULL,
                 kind    TEXT NOT NULL,
                 file_id INTEGER NOT NULL,
                 path    TEXT NOT NULL,
                 line    INTEGER NOT NULL,
                 col     INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_sym_name    ON symbols(name);
             CREATE INDEX IF NOT EXISTS idx_sym_kn      ON symbols(kind, name);
             CREATE INDEX IF NOT EXISTS idx_sym_file_id ON symbols(file_id);",
        )
        .map_err(|e| IndexError::CorruptIndex(format!("symbol db schema: {e}")))?;
        Ok(SymbolIndex { conn: Mutex::new(conn) })
    }

    /// Delete all symbols for the given file_ids (used before re-indexing).
    ///
    /// Batches deletes in chunks of 999 to stay within SQLite's default
    /// SQLITE_MAX_VARIABLE_NUMBER limit.
    pub fn delete_for_files(&self, file_ids: &[u32]) -> Result<(), IndexError> {
        if file_ids.is_empty() {
            return Ok(());
        }
        const SQLITE_MAX_PARAMS: usize = 999;
        // Do not recover from a poisoned mutex: the connection may hold an open
        // transaction or have inconsistent prepared-statement cache state, and
        // reusing it risks silent symbol index corruption.
        let conn = self
            .conn
            .lock()
            .map_err(|_| IndexError::CorruptIndex("symbol db mutex poisoned".into()))?;
        for chunk in file_ids.chunks(SQLITE_MAX_PARAMS) {
            let placeholders: String =
                chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!("DELETE FROM symbols WHERE file_id IN ({placeholders})");
            let params: Vec<&dyn rusqlite::ToSql> =
                chunk.iter().map(|id| id as &dyn rusqlite::ToSql).collect();
            conn.execute(&sql, params.as_slice())
                .map_err(|e| IndexError::CorruptIndex(format!("symbol delete: {e}")))?;
        }
        Ok(())
    }

    /// Extract and insert symbols for a single file.
    pub fn index_file(
        &self,
        file_id: u32,
        path: &str,
        content: &[u8],
    ) -> Result<(), IndexError> {
        let symbols = extract_symbols(path, content);
        if symbols.is_empty() {
            return Ok(());
        }
        // Do not recover from a poisoned mutex (same rationale as delete_for_files).
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| IndexError::CorruptIndex("symbol db mutex poisoned".into()))?;
        // Use transaction() rather than unchecked_transaction(): the checked variant
        // verifies no active transaction exists on the connection, catching cases
        // where a previous panic left a transaction open.
        let tx = conn
            .transaction()
            .map_err(|e| IndexError::CorruptIndex(format!("symbol tx: {e}")))?;
        {
            let mut stmt = tx
                .prepare_cached(
                    "INSERT INTO symbols(name, kind, file_id, path, line, col)
                     VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                )
                .map_err(|e| IndexError::CorruptIndex(format!("symbol stmt: {e}")))?;
            for sym in &symbols {
                stmt.execute(params![
                    sym.name,
                    sym.kind.as_str(),
                    file_id,
                    path,
                    sym.line,
                    sym.column,
                ])
                .map_err(|e| IndexError::CorruptIndex(format!("symbol insert: {e}")))?;
            }
        }
        tx.commit()
            .map_err(|e| IndexError::CorruptIndex(format!("symbol commit: {e}")))?;
        Ok(())
    }

    /// Search for symbols matching `name_query` (prefix match, case-insensitive).
    ///
    /// Optionally filter by `kind` (e.g., "function", "struct").
    pub fn search(
        &self,
        name_query: &str,
        kind_filter: Option<SymbolKind>,
    ) -> Result<Vec<SearchMatch>, IndexError> {
        // Do not recover from a poisoned mutex (same rationale as delete_for_files).
        let conn = self
            .conn
            .lock()
            .map_err(|_| IndexError::CorruptIndex("symbol db mutex poisoned".into()))?;

        // Security: escape SQLite LIKE metacharacters before interpolation.
        //
        // SQLite LIKE recognises three metacharacters: '%' (any sequence), '_' (any
        // single character), and '\' (the escape character when ESCAPE '\' is set).
        // Without escaping, a user query like "f_o" matches "fXo", "foo", etc., and
        // a query like "fo%" matches everything starting with "fo" rather than the
        // literal string "fo%". The broadened result set is a correctness bug and a
        // potential information disclosure when results are used for access-control
        // decisions in automated tooling. We pair the escaped pattern with
        // `ESCAPE '\'` in the SQL so SQLite honours the escape sequences.
        let escaped = name_query
            .to_lowercase()
            .replace('\\', r"\\")
            .replace('%', r"\%")
            .replace('_', r"\_");
        let like_pat = format!("{escaped}%");

        let sql = if kind_filter.is_some() {
            "SELECT path, line, name FROM symbols \
             WHERE lower(name) LIKE ?1 ESCAPE '\\' AND kind = ?2 ORDER BY name, path LIMIT 1000"
        } else {
            "SELECT path, line, name FROM symbols \
             WHERE lower(name) LIKE ?1 ESCAPE '\\' ORDER BY name, path LIMIT 1000"
        };
        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| IndexError::CorruptIndex(format!("symbol search: {e}")))?;

        let rows: Vec<(String, u32, String)> = if let Some(kind) = kind_filter {
            stmt.query_map(params![like_pat, kind.as_str()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?, row.get::<_, String>(2)?))
            })
            .map_err(|e| IndexError::CorruptIndex(format!("symbol query: {e}")))?
            .filter_map(|r| r.ok())
            .collect()
        } else {
            stmt.query_map(params![like_pat], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?, row.get::<_, String>(2)?))
            })
            .map_err(|e| IndexError::CorruptIndex(format!("symbol query: {e}")))?
            .filter_map(|r| r.ok())
            .collect()
        };

        Ok(rows
            .into_iter()
            .map(|(path, line, name)| SearchMatch {
                path: PathBuf::from(path),
                line_number: line,
                line_content: name.into_bytes(),
                byte_offset: 0,
                submatch_start: 0,
                submatch_end: 0,
            })
            .collect())
    }
}

#[cfg(all(test, feature = "symbols"))]
mod tests {
    use super::*;
    use crate::symbol::extractor::SymbolKind;

    #[test]
    fn search_accepts_symbol_kind_filter() {
        // Compile-time check: search must accept Option<SymbolKind>.
        // This test failing to compile means the type regression was reintroduced.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let idx = SymbolIndex::open(tmp.path()).unwrap();
        let _r1 = idx.search("foo", Some(SymbolKind::Function));
        let _r2 = idx.search("bar", None);
        // If neither panics, the type system accepted Option<SymbolKind>.
        drop(_r1);
        drop(_r2);
    }
}

/// Parse a symbol search prefix (`sym:`, `def:`, `ref:`) from a pattern.
///
/// Returns `(name_query, kind_filter)` if the pattern has a symbol prefix, else `None`.
pub fn parse_symbol_prefix(pattern: &str) -> Option<(String, Option<SymbolKind>)> {
    if let Some(rest) = pattern.strip_prefix("sym:") {
        return Some((rest.to_string(), None));
    }
    if let Some(rest) = pattern.strip_prefix("def:") {
        return Some((rest.to_string(), Some(SymbolKind::Function)));
    }
    if let Some(rest) = pattern.strip_prefix("ref:") {
        return Some((rest.to_string(), None));
    }
    None
}
