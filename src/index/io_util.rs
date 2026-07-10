//! IO security helpers for safe file opens.
//!
//! These functions guard against TOCTOU races: `open_readonly_nofollow` uses
//! `O_NOFOLLOW` (where available) to block symlink substitution on the final
//! path component, and `verify_fd_matches_stat` checks that the opened fd
//! refers to the same inode that was stat'd before the open, catching
//! directory-component swaps that `O_NOFOLLOW` cannot block.

use std::path::Path;

/// Opens a file for reading without following symlinks on the final path component.
///
/// On Unix systems, uses `O_NOFOLLOW` to block symlink substitution on the final
/// path component. However, `O_NOFOLLOW` cannot protect against directory-component
/// swaps that occur between the `stat()` call and the `open()` call.
/// Call `verify_fd_matches_stat` after opening to ensure the fd refers to the
/// same inode that was stat'd before the open.
///
/// # Security: why `libc::O_NOFOLLOW` and not a hardcoded constant
///
/// The numeric value of `O_NOFOLLOW` is NOT uniform across Linux architectures:
///   - x86-64 and AArch64 Linux: 0x20000 (0o400000)
///   - MIPS Linux:                0x400
///   - SPARC Linux:               0x20
///   - macOS / *BSD:              0x100
///
/// A hardcoded constant (e.g. `0o400000`) compiles silently with the wrong
/// value on MIPS cross-compile targets, disabling the symlink guard with no
/// warning or error. `libc::O_NOFOLLOW` is always the correct value for the
/// compilation target ABI.
#[cfg(unix)]
pub fn open_readonly_nofollow(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

// Non-Unix / WASM fallback: O_NOFOLLOW and inode verification are unavailable.
// The WASM build bypasses the filesystem entirely (callers provide content
// directly via WasmIndex::new), so TOCTOU mitigations are not applicable.
// Windows: FILE_FLAG_OPEN_REPARSE_POINT could block symlink traversal on the
// final component (analogous to O_NOFOLLOW), but requires CreateFileW via
// windows-sys. For now, fall back to plain File::open on non-Unix.
#[cfg(not(unix))]
/// Fallback implementation of `open_readonly_nofollow` for non-Unix platforms (e.g. Windows, WASM).
///
/// Since `O_NOFOLLOW` is not available, this falls back to a standard read-only file open.
pub fn open_readonly_nofollow(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    std::fs::File::open(path)
}

/// Verify that the fd we just opened refers to the same inode we stat'd
/// before the open. This catches directory-component symlink swaps that
/// happen in the window between canonicalize() and open(): O_NOFOLLOW only
/// blocks the final path component, not intermediate ones.
#[cfg(unix)]
pub fn verify_fd_matches_stat(file: &std::fs::File, pre_open_meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    match file.metadata() {
        Ok(fd_meta) => fd_meta.dev() == pre_open_meta.dev() && fd_meta.ino() == pre_open_meta.ino(),
        Err(_) => false,
    }
}

/// Windows verification for `verify_fd_matches_stat` using `GetFileInformationByHandle`.
///
/// Compares the volume serial number and 64-bit file index from the open handle
/// against the pre-opened file's metadata to detect TOCTOU path swapping.
#[cfg(windows)]
pub fn verify_fd_matches_stat(file: &std::fs::File, pre_open_meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    use std::os::windows::io::AsRawHandle;

    let handle = file.as_raw_handle();
    if handle.is_null() || handle as isize == -1 {
        return false;
    }

    // Query information for the open file handle.
    let mut info = unsafe {
        std::mem::zeroed::<windows_sys::Win32::Storage::FileSystem::BY_HANDLE_FILE_INFORMATION>()
    };
    let ok = unsafe {
        windows_sys::Win32::Storage::FileSystem::GetFileInformationByHandle(handle as _, &mut info)
    };
    if ok == 0 {
        return false;
    }

    let fd_volume = info.dwVolumeSerialNumber;
    let fd_index = ((info.nFileIndexHigh as u64) << 32) | (info.nFileIndexLow as u64);

    // Compare with the pre-opened metadata fields.
    match (
        pre_open_meta.volume_serial_number(),
        pre_open_meta.file_index(),
    ) {
        (Some(meta_volume), Some(meta_index)) => fd_volume == meta_volume && fd_index == meta_index,
        _ => false,
    }
}

/// WASM stub: filesystem is not used (WasmIndex receives content directly),
/// so TOCTOU verification is not applicable.
#[cfg(not(any(unix, windows)))]
#[allow(dead_code)]
pub fn verify_fd_matches_stat(_file: &std::fs::File, _pre_open_meta: &std::fs::Metadata) -> bool {
    true
}

/// Open a repo-root anchor handle for the openat2 fast path, or `None` when the
/// fast path is unavailable (non-Linux, a kernel without `openat2(2)`, or the
/// `SYNTEXT_NO_OPENAT2` kill switch). Open this once per search and pass the
/// result to [`open_beneath`] for every candidate; it is an `O_PATH` directory
/// fd, never used for I/O itself.
#[cfg(target_os = "linux")]
pub(crate) fn open_root_dirfd(canonical_root: &Path) -> Option<std::fs::File> {
    linux_openat2::open_root_dirfd(canonical_root)
}

/// Non-Linux platforms have no `openat2(2)`; callers always take the portable
/// hardened path in [`open_beneath`].
#[cfg(not(target_os = "linux"))]
pub(crate) fn open_root_dirfd(_canonical_root: &Path) -> Option<std::fs::File> {
    None
}

/// Open `rel` (a repo-relative path) for reading, guaranteed to resolve beneath
/// `canonical_root`.
///
/// On Linux with a valid `root_fd` (from [`open_root_dirfd`]), this uses a
/// single `openat2(2)` with `RESOLVE_BENEATH`, which the kernel enforces
/// atomically -- collapsing the portable path's canonicalize + stat + open +
/// fd-verify sequence into one syscall and closing the intermediate-component
/// TOCTOU window that sequence cannot fully eliminate. On any fast-path failure
/// (older kernel, or a candidate `RESOLVE_BENEATH` refuses such as an absolute
/// symlink whose target is still inside the repo), and on every non-Linux
/// platform, it falls back to [`legacy_open_checked`]. Returns `None` on
/// containment failure or I/O error -- the same skip-this-doc signal the
/// portable path produced before.
pub(crate) fn open_beneath(
    root_fd: Option<&std::fs::File>,
    canonical_root: &Path,
    rel: &Path,
) -> Option<std::fs::File> {
    #[cfg(target_os = "linux")]
    if let Some(dirfd) = root_fd {
        match linux_openat2::try_open_beneath(dirfd, rel) {
            // Kernel resolved and opened it beneath the root: trust it, no
            // separate canonicalize/stat/verify needed.
            linux_openat2::Outcome::Opened(file) => return Some(file),
            // Kernel lacks openat2, or this candidate needs the portable path:
            // fall through. Neither case is a security downgrade -- legacy still
            // enforces containment.
            linux_openat2::Outcome::Unsupported | linux_openat2::Outcome::CandidateRejected => {}
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = root_fd; // no openat2 off Linux

    legacy_open_checked(canonical_root, rel)
}

/// Convenience wrapper over [`open_beneath`] for non-hot call sites (commit,
/// render) that open one file at a time rather than a batch: it opens a fresh
/// repo-root anchor fd for this single open. The hot search path instead opens
/// the anchor once per query via [`open_root_dirfd`] and reuses it across every
/// candidate. Same containment guarantees and `None`-on-failure semantics.
pub(crate) fn open_beneath_fresh(canonical_root: &Path, rel: &Path) -> Option<std::fs::File> {
    let root_fd = open_root_dirfd(canonical_root);
    open_beneath(root_fd.as_ref(), canonical_root, rel)
}

/// Portable hardened open of `rel` beneath `canonical_root`: canonicalize,
/// containment (`starts_with`) check, stat, `O_NOFOLLOW` open, then verify the
/// opened fd's inode matches the stat (defeating final- and intermediate-
/// component symlink swaps). Returns `None` on containment failure or any I/O
/// error. This is the fallback for platforms without `openat2(2)` and for
/// candidates the fast path refuses; it is a verbatim extraction of the check
/// that previously lived inline in `search::resolver::resolve_doc`.
fn legacy_open_checked(canonical_root: &Path, rel: &Path) -> Option<std::fs::File> {
    let abs_path = canonical_root.join(rel);
    let canonical = std::fs::canonicalize(&abs_path).ok()?;
    if !canonical.starts_with(canonical_root) {
        return None;
    }
    #[cfg(any(unix, windows))]
    {
        let pre_meta = std::fs::metadata(&canonical).ok()?;
        let file = open_readonly_nofollow(&canonical).ok()?;
        if !verify_fd_matches_stat(&file, &pre_meta) {
            return None;
        }
        Some(file)
    }
    #[cfg(not(any(unix, windows)))]
    {
        open_readonly_nofollow(&canonical).ok()
    }
}

/// Linux `openat2(2)` fast path. Kept in a module so all of its raw-syscall
/// machinery, availability state, and `unsafe` are localized.
#[cfg(target_os = "linux")]
mod linux_openat2 {
    use std::path::Path;
    use std::sync::atomic::{AtomicU8, Ordering};

    // Process-wide openat2 availability. Probed lazily on first use: once a
    // kernel returns ENOSYS/EINVAL (no openat2, or a resolve flag it doesn't
    // know), every later call skips straight to the portable path. Per-candidate
    // failures (ELOOP, EXDEV, absolute-symlink-under-BENEATH, ...) must NOT flip
    // this -- they are about one path, not the syscall's availability.
    static STATE: AtomicU8 = AtomicU8::new(UNPROBED);
    const UNPROBED: u8 = 0;
    const AVAILABLE: u8 = 1;
    const UNAVAILABLE: u8 = 2;

    pub(super) enum Outcome {
        /// Opened beneath the root; the fd is owned by the returned File.
        Opened(std::fs::File),
        /// Kernel has no usable openat2; use the portable path from now on.
        Unsupported,
        /// This specific candidate must use the portable path (state untouched).
        CandidateRejected,
    }

    /// Open an `O_PATH` handle to the (already-canonical) repo root, or `None`
    /// when the fast path is disabled by the kill switch or a prior probe.
    pub(super) fn open_root_dirfd(canonical_root: &Path) -> Option<std::fs::File> {
        use std::os::unix::fs::OpenOptionsExt;

        // Escape hatch + CI knob: forces the portable branch even on an
        // openat2-capable kernel so both code paths are exercised on one host.
        if std::env::var_os("SYNTEXT_NO_OPENAT2").is_some() {
            return None;
        }
        if STATE.load(Ordering::Relaxed) == UNAVAILABLE {
            return None;
        }
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC)
            .open(canonical_root)
            .ok()
    }

    /// Attempt one `openat2(dirfd, rel, RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS)`.
    pub(super) fn try_open_beneath(dirfd: &std::fs::File, rel: &Path) -> Outcome {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::io::{AsRawFd, FromRawFd, RawFd};

        if STATE.load(Ordering::Relaxed) == UNAVAILABLE {
            return Outcome::Unsupported;
        }
        let Ok(c_path) = std::ffi::CString::new(rel.as_os_str().as_bytes()) else {
            // Interior NUL cannot be a real path component; let legacy reject it.
            return Outcome::CandidateRejected;
        };
        // `open_how` is #[non_exhaustive], so it cannot be struct-literalled.
        // SAFETY: it is a repr(C) struct of three u64 integer fields; an
        // all-zero bit pattern is a valid, inert instance (flags=0, mode=0,
        // resolve=0) that we then fill in.
        let mut how: libc::open_how = unsafe { std::mem::zeroed() };
        how.flags = (libc::O_RDONLY | libc::O_CLOEXEC) as u64;
        // BENEATH: every resolved component (incl. `..` and symlink targets) must
        // stay under dirfd -- atomic containment. NO_MAGICLINKS blocks /proc
        // magic-symlink escapes. NOT NO_SYMLINKS: the index legitimately contains
        // symlinks whose canonical target is inside the repo.
        how.resolve = libc::RESOLVE_BENEATH | libc::RESOLVE_NO_MAGICLINKS;
        // openat2 may return EAGAIN when resolution races a concurrent mount
        // table change; the man page instructs callers to retry. Bound it.
        for _ in 0..2 {
            // SAFETY: direct `openat2(2)` syscall. `dirfd` is a live borrowed
            // O_PATH directory fd that outlives this call. `c_path` is a valid
            // NUL-terminated C string kept alive across the call. `&how` points
            // to a fully-initialised `open_how` and we pass its exact byte size.
            // On success the kernel returns a brand-new fd (>= 0) we take sole
            // ownership of; on error it returns -1 and sets errno.
            let ret = unsafe {
                libc::syscall(
                    libc::SYS_openat2,
                    dirfd.as_raw_fd(),
                    c_path.as_ptr(),
                    &how as *const libc::open_how,
                    std::mem::size_of::<libc::open_how>(),
                )
            };
            if ret >= 0 {
                STATE.store(AVAILABLE, Ordering::Relaxed);
                // SAFETY: `ret` is a fresh, valid fd owned exclusively by us.
                return Outcome::Opened(unsafe { std::fs::File::from_raw_fd(ret as RawFd) });
            }
            match std::io::Error::last_os_error().raw_os_error().unwrap_or(0) {
                // No openat2 (kernel < 5.6), or a resolve flag the kernel does
                // not understand: disable the fast path process-wide.
                libc::ENOSYS | libc::EINVAL => {
                    STATE.store(UNAVAILABLE, Ordering::Relaxed);
                    return Outcome::Unsupported;
                }
                libc::EAGAIN => continue,
                // ELOOP / EXDEV / EACCES / ENOENT / absolute-symlink-under-BENEATH
                // / etc.: this candidate needs the portable path (or is genuinely
                // unopenable). Do not poison the global state.
                _ => return Outcome::CandidateRejected,
            }
        }
        Outcome::CandidateRejected // EAGAIN retries exhausted
    }
}
