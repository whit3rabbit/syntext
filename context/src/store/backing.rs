//! Where a segment's bytes live: a file read with positional reads, or an
//! in-memory buffer (the live conversation-memory segment, tests).
//!
//! File-backed segments are read with `pread`-style positional reads instead
//! of a memory map. Large sections (passage text, postings) are only read
//! for the candidates a query touches, so resident memory stays proportional
//! to the metadata, and an external truncation of a segment file surfaces as
//! an I/O error rather than a SIGBUS. It also keeps the crate free of
//! `unsafe`.

use std::borrow::Cow;
use std::fs::File;
use std::io;
use std::sync::Arc;

use crate::error::{ContextError, Result};

pub(crate) enum Backing {
    Memory(Arc<[u8]>),
    File { file: PositionalFile, len: u64 },
}

impl Backing {
    pub(crate) fn file(file: File) -> io::Result<Self> {
        let len = file.metadata()?.len();
        Ok(Backing::File {
            file: PositionalFile::new(file),
            len,
        })
    }

    pub(crate) fn len(&self) -> u64 {
        match self {
            Backing::Memory(b) => b.len() as u64,
            Backing::File { len, .. } => *len,
        }
    }

    /// Read `len` bytes at `off`. Out-of-range requests are corruption.
    pub(crate) fn read_at(&self, off: u64, len: usize) -> Result<Cow<'_, [u8]>> {
        let end = off
            .checked_add(len as u64)
            .filter(|&e| e <= self.len())
            .ok_or_else(|| ContextError::corrupt("read past end of segment"))?;
        match self {
            Backing::Memory(b) => Ok(Cow::Borrowed(&b[off as usize..end as usize])),
            Backing::File { file, .. } => {
                let mut buf = vec![0u8; len];
                file.read_exact_at(&mut buf, off)?;
                Ok(Cow::Owned(buf))
            }
        }
    }
}

/// A file supporting `&self` positional reads on every platform.
pub(crate) struct PositionalFile {
    #[cfg(any(unix, windows))]
    file: File,
    #[cfg(not(any(unix, windows)))]
    file: std::sync::Mutex<File>,
}

impl PositionalFile {
    fn new(file: File) -> Self {
        #[cfg(any(unix, windows))]
        {
            PositionalFile { file }
        }
        #[cfg(not(any(unix, windows)))]
        {
            PositionalFile {
                file: std::sync::Mutex::new(file),
            }
        }
    }

    #[cfg(unix)]
    fn read_exact_at(&self, buf: &mut [u8], off: u64) -> io::Result<()> {
        std::os::unix::fs::FileExt::read_exact_at(&self.file, buf, off)
    }

    #[cfg(windows)]
    fn read_exact_at(&self, mut buf: &mut [u8], mut off: u64) -> io::Result<()> {
        use std::os::windows::fs::FileExt;
        while !buf.is_empty() {
            match self.file.seek_read(buf, off) {
                Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Ok(n) => {
                    buf = &mut buf[n..];
                    off += n as u64;
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    #[cfg(not(any(unix, windows)))]
    fn read_exact_at(&self, buf: &mut [u8], off: u64) -> io::Result<()> {
        use std::io::{Read, Seek, SeekFrom};
        let mut f = self.file.lock().unwrap_or_else(|e| e.into_inner());
        f.seek(SeekFrom::Start(off))?;
        f.read_exact(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn memory_and_file_backings_agree() {
        let data: Vec<u8> = (0..=255u8).collect();
        let mem = Backing::Memory(Arc::from(data.clone()));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("seg");
        std::fs::File::create(&path)
            .unwrap()
            .write_all(&data)
            .unwrap();
        let file = Backing::file(File::open(&path).unwrap()).unwrap();
        for b in [&mem, &file] {
            assert_eq!(b.len(), 256);
            assert_eq!(&*b.read_at(10, 5).unwrap(), &data[10..15]);
            assert_eq!(&*b.read_at(256, 0).unwrap(), &[] as &[u8]);
            assert!(b.read_at(250, 7).is_err());
            assert!(b.read_at(u64::MAX, 2).is_err());
        }
        drop(file);
    }
}
