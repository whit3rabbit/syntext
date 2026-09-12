//! Binary serialization and deserialization for the persistent change catalogue.
//!
//! Format:
//! ```text
//! [0..8]   Magic: "SNTXCAT1"
//! [8..12]  Version: 1 (u32 LE)
//! [12..20] Generation: current generation (u64 LE)
//! [20..28] Payload Checksum: xxHash64 over remaining bytes (u64 LE)
//! [28..32] File Entry Count: N (u32 LE)
//! N File Entries:
//!   - path len: u16 LE
//!   - path bytes: [u8; len] (UTF-8)
//!   - digest: [u8; 32]
//!   - size: u64 LE
//!   - mtime_secs: i64 LE
//!   - mtime_nanos: u32 LE
//!   - observed_generation: u64 LE
//! [..] Consumer Entry Count: M (u32 LE)
//! M Consumer Entries:
//!   - name len: u16 LE
//!   - name bytes: [u8; len] (UTF-8)
//!   - applied_generation: u64 LE
//! ```

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use super::fingerprint::{ContentDigest, FileFingerprint};

const MAGIC: &[u8; 8] = b"SNTXCAT1";
const VERSION: u32 = 1;

/// In-memory record stored in the catalogue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogueEntry {
    /// File content and metadata fingerprint.
    pub fingerprint: FileFingerprint,
    /// Generation in which this fingerprint was observed.
    pub observed_generation: u64,
}

/// Decoded catalogue state read from disk.
#[derive(Debug)]
pub struct DecodedCatalogue {
    /// Current monotonic generation.
    pub generation: u64,
    /// File entries by relative path.
    pub entries: HashMap<PathBuf, CatalogueEntry>,
    /// Consumer progress acknowledgements.
    pub consumers: HashMap<String, u64>,
}

/// Read the catalogue from `path`. Returns `None` if the file does not exist.
pub fn read_catalogue(path: &Path) -> io::Result<Option<DecodedCatalogue>> {
    if !path.exists() {
        return Ok(None);
    }
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);

    let mut magic = [0u8; 8];
    reader.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid catalogue magic header",
        ));
    }

    let mut ver_bytes = [0u8; 4];
    reader.read_exact(&mut ver_bytes)?;
    let version = u32::from_le_bytes(ver_bytes);
    if version != VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported catalogue version: {version}"),
        ));
    }

    let mut gen_bytes = [0u8; 8];
    reader.read_exact(&mut gen_bytes)?;
    let generation = u64::from_le_bytes(gen_bytes);

    let mut checksum_bytes = [0u8; 8];
    reader.read_exact(&mut checksum_bytes)?;
    let expected_checksum = u64::from_le_bytes(checksum_bytes);

    let mut payload = Vec::new();
    reader.read_to_end(&mut payload)?;

    let actual_checksum = xxhash_rust::xxh64::xxh64(&payload, 0);
    if actual_checksum != expected_checksum {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "catalogue payload checksum mismatch (corrupted or truncated)",
        ));
    }

    let mut cur = &payload[..];

    let entry_count = read_u32(&mut cur)?;
    let mut entries = HashMap::with_capacity(entry_count as usize);
    for _ in 0..entry_count {
        let path_len = read_u16(&mut cur)? as usize;
        let path_bytes = read_slice(&mut cur, path_len)?;
        let path_str = std::str::from_utf8(path_bytes).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid utf-8 path: {e}"),
            )
        })?;
        let path = PathBuf::from(path_str);

        let digest_bytes = read_slice(&mut cur, 32)?;
        let mut raw_digest = [0u8; 32];
        raw_digest.copy_from_slice(digest_bytes);
        let digest = ContentDigest::from_raw(raw_digest);

        let size = read_u64(&mut cur)?;
        let mtime_secs = read_i64(&mut cur)?;
        let mtime_nanos = read_u32(&mut cur)?;
        let observed_gen = read_u64(&mut cur)?;

        entries.insert(
            path,
            CatalogueEntry {
                fingerprint: FileFingerprint::new(digest, size, mtime_secs, mtime_nanos),
                observed_generation: observed_gen,
            },
        );
    }

    let consumer_count = read_u32(&mut cur)?;
    let mut consumers = HashMap::with_capacity(consumer_count as usize);
    for _ in 0..consumer_count {
        let name_len = read_u16(&mut cur)? as usize;
        let name_bytes = read_slice(&mut cur, name_len)?;
        let name = std::str::from_utf8(name_bytes)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
            .to_string();
        let applied_gen = read_u64(&mut cur)?;
        consumers.insert(name, applied_gen);
    }

    Ok(Some(DecodedCatalogue {
        generation,
        entries,
        consumers,
    }))
}

/// Atomically write the catalogue to `path` via a temporary file and rename.
pub fn write_catalogue(
    path: &Path,
    generation: u64,
    entries: &HashMap<PathBuf, CatalogueEntry>,
    consumers: &HashMap<String, u64>,
) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let temp_path = parent.join(format!(
        ".catalogue-{}-{}.tmp",
        std::process::id(),
        generation
    ));

    let file = File::create(&temp_path)?;
    let mut writer = BufWriter::new(file);

    let mut payload = Vec::new();
    payload.extend_from_slice(&(entries.len() as u32).to_le_bytes());

    // Sort paths for deterministic serialization.
    let mut sorted_paths: Vec<&PathBuf> = entries.keys().collect();
    sorted_paths.sort_unstable();

    for p in sorted_paths {
        let entry = &entries[p];
        let p_str = p.to_string_lossy();
        let p_bytes = p_str.as_bytes();
        payload.extend_from_slice(&(p_bytes.len() as u16).to_le_bytes());
        payload.extend_from_slice(p_bytes);
        payload.extend_from_slice(entry.fingerprint.digest.as_bytes());
        payload.extend_from_slice(&entry.fingerprint.size.to_le_bytes());
        payload.extend_from_slice(&entry.fingerprint.mtime_secs.to_le_bytes());
        payload.extend_from_slice(&entry.fingerprint.mtime_nanos.to_le_bytes());
        payload.extend_from_slice(&entry.observed_generation.to_le_bytes());
    }

    payload.extend_from_slice(&(consumers.len() as u32).to_le_bytes());
    let mut sorted_consumers: Vec<(&String, &u64)> = consumers.iter().collect();
    sorted_consumers.sort_unstable_by(|a, b| a.0.cmp(b.0));

    for (name, &gen) in sorted_consumers {
        let name_bytes = name.as_bytes();
        payload.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        payload.extend_from_slice(name_bytes);
        payload.extend_from_slice(&gen.to_le_bytes());
    }

    let checksum = xxhash_rust::xxh64::xxh64(&payload, 0);

    writer.write_all(MAGIC)?;
    writer.write_all(&VERSION.to_le_bytes())?;
    writer.write_all(&generation.to_le_bytes())?;
    writer.write_all(&checksum.to_le_bytes())?;
    writer.write_all(&payload)?;
    writer.flush()?;
    drop(writer);

    fs::rename(&temp_path, path)
}

fn read_u16(cur: &mut &[u8]) -> io::Result<u16> {
    let slice = read_slice(cur, 2)?;
    Ok(u16::from_le_bytes([slice[0], slice[1]]))
}

fn read_u32(cur: &mut &[u8]) -> io::Result<u32> {
    let slice = read_slice(cur, 4)?;
    Ok(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn read_i64(cur: &mut &[u8]) -> io::Result<i64> {
    let slice = read_slice(cur, 8)?;
    Ok(i64::from_le_bytes([
        slice[0], slice[1], slice[2], slice[3], slice[4], slice[5], slice[6], slice[7],
    ]))
}

fn read_u64(cur: &mut &[u8]) -> io::Result<u64> {
    let slice = read_slice(cur, 8)?;
    Ok(u64::from_le_bytes([
        slice[0], slice[1], slice[2], slice[3], slice[4], slice[5], slice[6], slice[7],
    ]))
}

fn read_slice<'a>(cur: &mut &'a [u8], len: usize) -> io::Result<&'a [u8]> {
    if cur.len() < len {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "unexpected eof reading catalogue record",
        ));
    }
    let (head, tail) = cur.split_at(len);
    *cur = tail;
    Ok(head)
}

#[cfg(test)]
#[path = "codec_tests.rs"]
mod tests;
