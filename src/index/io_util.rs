//! IO security helpers for safe file opens.
//!
//! These functions guard against TOCTOU races: `open_readonly_nofollow` uses
//! `O_NOFOLLOW` (where available) to block symlink substitution on the final
//! path component, and `verify_fd_matches_stat` checks that the opened fd
//! refers to the same inode that was stat'd before the open, catching
//! directory-component swaps that `O_NOFOLLOW` cannot block.

#[cfg(unix)]
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

/// Windows implementation of `verify_fd_matches_stat`.
///
/// Uses `GetFileInformationByHandle` (exposed via `std::os::windows::fs::MetadataExt`)
/// to compare volume serial number + file index between the pre-open stat and the
/// opened handle. This is the Windows equivalent of Unix dev+ino comparison.
///
/// Degrades to `true` (no protection) if either metadata object lacks handle
/// information, which can happen when `pre_open_meta` was obtained via
/// `symlink_metadata` on older Rust versions where that call uses
/// `GetFileAttributesExW` instead of opening a handle.
#[cfg(windows)]
pub fn verify_fd_matches_stat(file: &std::fs::File, pre_open_meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    match file.metadata() {
        Ok(fd_meta) => {
            match (
                fd_meta.file_index(),
                pre_open_meta.file_index(),
                fd_meta.volume_serial_number(),
                pre_open_meta.volume_serial_number(),
            ) {
                (Some(fd_idx), Some(pre_idx), Some(fd_vol), Some(pre_vol)) => {
                    fd_idx == pre_idx && fd_vol == pre_vol
                }
                // Cannot verify: metadata lacks handle info. Allow the open
                // rather than blocking all file reads on affected code paths.
                _ => true,
            }
        }
        Err(_) => false,
    }
}

/// WASM stub: filesystem is not used (WasmIndex receives content directly),
/// so TOCTOU verification is not applicable.
#[cfg(not(any(unix, windows)))]
#[allow(dead_code)]
pub fn verify_fd_matches_stat(_file: &std::fs::File, _pre_open_meta: &std::fs::Metadata) -> bool {
    true
}
