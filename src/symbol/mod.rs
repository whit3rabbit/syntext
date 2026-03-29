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

fn initialize_schema(conn: &Connection) -> Result<(), IndexError> {
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
    .map_err(|e| IndexError::CorruptIndex(format!("symbol db schema: {e}")))
}

impl SymbolIndex {
    /// Open or create a symbol index at `db_path`.
    pub fn open(db_path: &Path) -> Result<Self, IndexError> {
        let conn = Connection::open(db_path)
            .map_err(|e| IndexError::CorruptIndex(format!("symbol db: {e}")))?;
        initialize_schema(&conn)?;
        Ok(SymbolIndex {
            conn: Mutex::new(conn),
        })
    }

    pub(crate) fn reopen(&self, db_path: &Path) -> Result<(), IndexError> {
        let conn = Connection::open(db_path)
            .map_err(|e| IndexError::CorruptIndex(format!("symbol db: {e}")))?;
        initialize_schema(&conn)?;
        let mut current = self
            .conn
            .lock()
            .map_err(|_| IndexError::CorruptIndex("symbol db mutex poisoned".into()))?;
        *current = conn;
        Ok(())
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
            let placeholders: String = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!("DELETE FROM symbols WHERE file_id IN ({placeholders})");
            let params: Vec<&dyn rusqlite::ToSql> =
                chunk.iter().map(|id| id as &dyn rusqlite::ToSql).collect();
            conn.execute(&sql, params.as_slice())
                .map_err(|e| IndexError::CorruptIndex(format!("symbol delete: {e}")))?;
        }
        Ok(())
    }

    /// Extract and insert symbols for a single file.
    pub fn index_file(&self, file_id: u32, path: &str, content: &[u8]) -> Result<(), IndexError> {
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
    ///
    /// Security audit (SQL injection): all user-supplied values (`name_query`,
    /// `kind_filter`) are bound via `rusqlite::params!` positional placeholders
    /// (`?1`, `?2`). The only dynamic SQL is the branch selecting one of two
    /// static string literals for the `kind` clause. No user input is ever
    /// interpolated into the SQL text. LIKE metacharacters are escaped below.
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

        // SAFETY INVARIANT: `kind_filter` is never interpolated into the SQL
        // string. It is always bound via `?2` positional parameter. If a future
        // change needs to vary the SQL based on kind_filter's VALUE (not just
        // its presence), it must remain parameterized. Interpolating kind_filter
        // into the SQL string would bypass the LIKE escaping above and enable
        // SQL injection via crafted SymbolKind variants (if the enum ever gains
        // user-controlled string payloads).
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
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, u32>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|e| IndexError::CorruptIndex(format!("symbol query: {e}")))?
            .filter_map(|r| r.ok())
            .collect()
        } else {
            stmt.query_map(params![like_pat], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, u32>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|e| IndexError::CorruptIndex(format!("symbol query: {e}")))?
            .filter_map(|r| r.ok())
            .collect()
        };

        Ok(rows
            .into_iter()
            .filter_map(|(path, line, name)| {
                let pb = PathBuf::from(&path);
                // Security: all paths written by index_file() are repo-relative paths
                // from the walk — no absolute paths, no '..' components. A crafted
                // symbols.db placed in the index directory could embed traversal paths;
                // a caller that joins the result with repo_root would escape the repo.
                // We filter (skip) rather than error so one bad row does not abort
                // a query that has many valid rows.
                if pb.is_absolute()
                    || pb
                        .components()
                        .any(|c| c == std::path::Component::ParentDir)
                {
                    return None;
                }
                Some(SearchMatch {
                    path: pb,
                    line_number: line,
                    line_content: name.into_bytes(),
                    byte_offset: 0,
                    submatch_start: 0,
                    submatch_end: 0,
                })
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

    #[test]
    fn search_filters_traversal_paths_from_crafted_db() {
        // Security regression test for Fix 2 (Vuln 4): a crafted symbols.db
        // that embeds path traversal strings must not appear in search results.
        // Legitimate rows with valid relative paths must still be returned.
        use rusqlite::params;

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let idx = SymbolIndex::open(tmp.path()).unwrap();

        // Inject rows directly, bypassing index_file(), to simulate a crafted DB.
        {
            let conn = idx.conn.lock().unwrap();
            // Bad: absolute path
            conn.execute(
                "INSERT INTO symbols(name, kind, file_id, path, line, col) \
                 VALUES('root_fn', 'function', 1, '/etc/passwd', 1, 0)",
                [],
            )
            .unwrap();
            // Bad: parent-dir traversal
            conn.execute(
                "INSERT INTO symbols(name, kind, file_id, path, line, col) \
                 VALUES('escape_fn', 'function', 2, '../../etc/shadow', 1, 0)",
                [],
            )
            .unwrap();
            // Good: legitimate relative path
            conn.execute(
                "INSERT INTO symbols(name, kind, file_id, path, line, col) \
                 VALUES('real_fn', 'function', 3, 'src/lib.rs', 10, 4)",
                params![],
            )
            .unwrap();
        }

        let results = idx.search("", None).unwrap();
        let paths: Vec<_> = results
            .iter()
            .map(|m| m.path.display().to_string())
            .collect();

        assert!(
            !paths
                .iter()
                .any(|p| p.contains("etc/passwd") || p.contains("etc/shadow")),
            "traversal paths must not appear in results: {:?}",
            paths
        );
        assert!(
            paths.iter().any(|p| p == "src/lib.rs"),
            "legitimate relative path must still be returned: {:?}",
            paths
        );
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
