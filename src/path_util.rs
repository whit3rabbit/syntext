use std::borrow::Cow;
use std::cmp::Ordering;
use std::path::{Path, PathBuf};

/// Compare two paths by their forward-slash byte components.
///
/// Reproduces `Path::cmp`'s component-wise ordering for normalized,
/// repo-relative forward-slash paths (no leading/trailing/double slashes, no
/// `.`/`..` or prefix components), while avoiding the `Components` iterator
/// overhead: it splits on `b'/'` and compares each segment as raw bytes, with a
/// shorter component-prefix path sorting first (e.g. `a` < `a/b`, and
/// `a/b` < `a.b` because segment `a` is a prefix of `a.b`). A raw byte memcmp
/// would instead order `a.b` < `a/b` (`.` < `/`), diverging from `Path::cmp`;
/// splitting on `/` is what keeps the order identical.
pub(crate) fn cmp_path_bytes(a: &Path, b: &Path) -> Ordering {
    let a = path_bytes(a);
    let b = path_bytes(b);
    let mut ai = a.split(|&c| c == b'/');
    let mut bi = b.split(|&c| c == b'/');
    loop {
        match (ai.next(), bi.next()) {
            (Some(x), Some(y)) => match x.cmp(y) {
                Ordering::Equal => continue,
                ord => return ord,
            },
            (Some(_), None) => return Ordering::Greater,
            (None, Some(_)) => return Ordering::Less,
            (None, None) => return Ordering::Equal,
        }
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    /// `cmp_path_bytes` must reproduce `Path::cmp` exactly for normalized
    /// repo-relative paths, including the `/`-vs-adjacent-byte edge cases where
    /// a raw byte memcmp would diverge (`.` = 0x2E < `/` = 0x2F).
    #[test]
    fn cmp_path_bytes_matches_path_cmp() {
        let paths = [
            "a",
            "a.b",
            "a/b",
            "a/b/c",
            "ab",
            "a/bc",
            "src/main.rs",
            "src/lib.rs",
            "src/index/mod.rs",
            "src/index.rs",
            "README.md",
            "Cargo.toml",
            "z",
            "",
        ];
        for x in paths {
            for y in paths {
                let px = Path::new(x);
                let py = Path::new(y);
                assert_eq!(
                    cmp_path_bytes(px, py),
                    px.cmp(py),
                    "cmp_path_bytes disagreed with Path::cmp for {x:?} vs {y:?}"
                );
            }
        }
    }
}
