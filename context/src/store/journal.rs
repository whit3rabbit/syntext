//! Append-only, checksummed journal of conversation turns.
//!
//! A turn is durable once its journal record is synced; it becomes
//! searchable immediately through the in-memory live segment, and is later
//! compacted into a regular segment. The manifest's `memory_through` marks
//! the highest sequence that a durable segment holds, and it only advances
//! in the same manifest write that publishes that segment, so a crash can
//! re-index turns but never lose them.
//!
//! Layout: a 16-byte header (`SXCTXJNL`, version, reserved) followed by
//! records of `[len: u32][xxh64(payload): u64][payload: JSON]`. A record
//! that is truncated or fails its checksum ends the journal: on a writable
//! open the torn tail is cut off, so later appends stay reachable.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};
use xxhash_rust::xxh64::xxh64;

use super::manifest::sync_dir;
use crate::error::{ContextError, Result};
use crate::types::{Role, Scope};

pub(crate) const JOURNAL_FILE: &str = "journal.log";
const MAGIC: &[u8; 8] = b"SXCTXJNL";
const VERSION: u32 = 1;
const HEADER_LEN: usize = 16;
const RECORD_HEAD: usize = 12;
/// Largest accepted turn payload.
pub(crate) const MAX_RECORD_BYTES: usize = 16 * 1024 * 1024;

/// One conversation turn as journaled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Turn {
    /// Store-wide, strictly increasing sequence number.
    pub seq: u64,
    /// Conversation session identifier.
    pub session: String,
    /// Speaker.
    pub role: Role,
    /// Visibility scope of the turn.
    pub scope: Scope,
    /// Message text.
    pub text: String,
    /// Unix seconds.
    pub timestamp: i64,
}

pub(crate) struct Journal {
    file: File,
    len: u64,
    sync: bool,
}

impl Journal {
    /// Open (creating if absent) and replay. A torn tail is truncated.
    pub(crate) fn open(dir: &Path, sync: bool) -> Result<(Journal, Vec<Turn>)> {
        let path = dir.join(JOURNAL_FILE);
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e.into()),
        };
        if bytes.len() < HEADER_LEN {
            // Absent, or torn while being created: start fresh.
            let journal = Journal::create(dir, &[], sync)?;
            return Ok((journal, Vec::new()));
        }
        let (turns, valid_end) = parse(&bytes)?;
        let file = OpenOptions::new().read(true).append(true).open(&path)?;
        if (valid_end as usize) < bytes.len() {
            log::warn!(
                "memory journal: truncating {} torn bytes after {} records",
                bytes.len() - valid_end as usize,
                turns.len()
            );
            file.set_len(valid_end)?;
            if sync {
                file.sync_all()?;
            }
        }
        Ok((
            Journal {
                file,
                len: valid_end,
                sync,
            },
            turns,
        ))
    }

    /// Replay without modifying the file (read-only handles). A concurrent
    /// writer's in-flight record simply ends the replay.
    pub(crate) fn read(dir: &Path) -> Result<Vec<Turn>> {
        match std::fs::read(dir.join(JOURNAL_FILE)) {
            Ok(b) if b.len() >= HEADER_LEN => Ok(parse(&b)?.0),
            Ok(_) => Ok(Vec::new()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e.into()),
        }
    }

    /// Write a fresh journal holding `turns`, atomically replacing any
    /// existing one.
    pub(crate) fn create(dir: &Path, turns: &[Turn], sync: bool) -> Result<Journal> {
        let mut buf = Vec::with_capacity(HEADER_LEN);
        buf.extend_from_slice(MAGIC);
        buf.extend_from_slice(&VERSION.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes());
        for t in turns {
            encode(t, &mut buf)?;
        }
        let tmp = dir.join(format!("journal-{}.tmp", uuid::Uuid::new_v4()));
        {
            let mut f = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
            f.write_all(&buf)?;
            if sync {
                f.sync_all()?;
            }
        }
        let path = dir.join(JOURNAL_FILE);
        if let Err(e) = std::fs::rename(&tmp, &path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e.into());
        }
        if sync {
            sync_dir(dir)?;
        }
        let file = OpenOptions::new().read(true).append(true).open(&path)?;
        Ok(Journal {
            file,
            len: buf.len() as u64,
            sync,
        })
    }

    /// Durably append one turn. On failure the file is cut back to its
    /// previous length so a partial record cannot strand later appends.
    pub(crate) fn append(&mut self, turn: &Turn) -> Result<()> {
        let mut buf = Vec::new();
        encode(turn, &mut buf)?;
        let res = self.file.write_all(&buf).and_then(|()| {
            if self.sync {
                self.file.sync_data()
            } else {
                Ok(())
            }
        });
        if let Err(e) = res {
            let _ = self.file.set_len(self.len);
            return Err(e.into());
        }
        self.len += buf.len() as u64;
        Ok(())
    }

    /// Replace the journal's contents with `keep` (rotation after the rest
    /// was compacted into a segment).
    pub(crate) fn rewrite(&mut self, dir: &Path, keep: &[Turn]) -> Result<()> {
        *self = Journal::create(dir, keep, self.sync)?;
        Ok(())
    }
}

fn encode(turn: &Turn, buf: &mut Vec<u8>) -> Result<()> {
    let payload = serde_json::to_vec(turn).map_err(|e| ContextError::invalid(e.to_string()))?;
    if payload.len() > MAX_RECORD_BYTES {
        return Err(ContextError::invalid("conversation turn exceeds 16 MiB"));
    }
    buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    buf.extend_from_slice(&xxh64(&payload, 0).to_le_bytes());
    buf.extend_from_slice(&payload);
    Ok(())
}

/// Parse a journal image; returns the valid turns and the byte offset where
/// valid data ends.
fn parse(bytes: &[u8]) -> Result<(Vec<Turn>, u64)> {
    if &bytes[..8] != MAGIC {
        return Err(ContextError::corrupt("bad memory journal magic"));
    }
    let version = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
    if version != VERSION {
        return Err(ContextError::UnsupportedVersion {
            found: version,
            supported: VERSION,
        });
    }
    let mut turns: Vec<Turn> = Vec::new();
    let mut pos = HEADER_LEN;
    while pos + RECORD_HEAD <= bytes.len() {
        let len = u32::from_le_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]])
            as usize;
        let mut sum = [0u8; 8];
        sum.copy_from_slice(&bytes[pos + 4..pos + 12]);
        let start = pos + RECORD_HEAD;
        let Some(payload) = bytes
            .get(start..start + len)
            .filter(|_| len <= MAX_RECORD_BYTES)
        else {
            break;
        };
        if xxh64(payload, 0) != u64::from_le_bytes(sum) {
            break;
        }
        let Ok(turn) = serde_json::from_slice::<Turn>(payload) else {
            break;
        };
        if turns.last().is_some_and(|prev| prev.seq >= turn.seq) {
            break;
        }
        turns.push(turn);
        pos = start + len;
    }
    Ok((turns, pos as u64))
}

#[cfg(test)]
#[path = "journal_tests.rs"]
mod tests;
