use std::borrow::Cow;
use std::path::{Path, PathBuf};

pub(crate) fn path_bytes(path: &Path) -> Cow<'_, [u8]> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;

        Cow::Borrowed(path.as_os_str().as_bytes())
    }
    #[cfg(not(unix))]
    {
        Cow::Owned(path.to_string_lossy().into_owned().into_bytes())
    }
}

/// Normalize a relative path to use forward-slash separators on all platforms.
/// On Unix this is a no-op. On Windows it replaces backslashes so that
/// byte-level matching in `path/filter.rs` (which splits on `b'/'`) works.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn normalize_to_forward_slashes(path: PathBuf) -> PathBuf {
    #[cfg(not(windows))]
    {
        path
    }
    #[cfg(windows)]
    {
        let s = path.to_string_lossy();
        if s.contains('\\') {
            PathBuf::from(s.replace('\\', "/"))
        } else {
            path
        }
    }
}

pub(crate) fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        PathBuf::from(OsString::from_vec(bytes.to_vec()))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
    }
}
