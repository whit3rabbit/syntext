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

// Non-Unix platforms (Windows, WASI) have no O_NOFOLLOW equivalent and no
// verify_fd_matches_stat, making the TOCTOU mitigations in commit_batch
// and resolver unavailable. Fail the build rather than ship a binary with
// silent security degradation.
//
// To add Windows support: implement CreateFileW with FILE_FLAG_OPEN_REPARSE_POINT
// + DeviceIoControl(FSCTL_GET_REPARSE_POINT) and a handle-based path check.
#[cfg(not(unix))]
compile_error!(
    "syntext requires a Unix target (Linux, macOS, *BSD). \
     Non-Unix builds lack O_NOFOLLOW and inode verification, \
     which are required for TOCTOU-safe file indexing. \
     See src/index/io_util.rs to add platform support."
);

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
