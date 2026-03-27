// Utility helpers for string processing and path handling
// Used throughout the search engine for normalization.

use std::path::{Path, PathBuf};

/// Normalize a file path by resolving `.` and `..` components.
pub fn normalize_path(path: &Path) -> PathBuf {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                components.pop();
            }
            std::path::Component::CurDir => {}
            other => components.push(other),
        }
    }
    components.iter().collect()
}

/// Strip ANSI escape codes from a string.
/// Useful when processing terminal output for indexing.
pub fn strip_ansi(input: &str) -> String {
    // Simple state machine approach
    let mut result = String::with_capacity(input.len());
    let mut in_escape = false;
    for ch in input.chars() {
        if in_escape {
            if ch.is_ascii_alphabetic() {
                in_escape = false;
            }
        } else if ch == '\x1b' {
            in_escape = true;
        } else {
            result.push(ch);
        }
    }
    result
}

/// Process a batch of file paths, filtering out non-existent ones.
/// Returns only paths that exist on disk.
pub fn process_batch(paths: &[PathBuf]) -> Vec<PathBuf> {
    paths.iter().filter(|p| p.exists()).cloned().collect()
}

/// Detect the likely programming language from a file extension.
pub fn detect_language(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()? {
        "rs" => Some("rust"),
        "py" => Some("python"),
        "ts" | "tsx" => Some("typescript"),
        "go" => Some("go"),
        "java" => Some("java"),
        "js" | "jsx" => Some("javascript"),
        "rb" => Some("ruby"),
        "c" | "h" => Some("c"),
        "cpp" | "hpp" => Some("cpp"),
        _ => None,
    }
}

/// Check if a string looks like a valid email address (basic heuristic).
/// Example matches: user@example.com, admin@192.168.1.1
pub fn looks_like_email(s: &str) -> bool {
    let parts: Vec<&str> = s.split('@').collect();
    parts.len() == 2 && !parts[0].is_empty() && parts[1].contains('.')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_ansi() {
        let input = "\x1b[31mhello\x1b[0m";
        assert_eq!(strip_ansi(input), "hello");
    }

    #[test]
    fn test_detect_language_rust() {
        let path = Path::new("src/main.rs");
        assert_eq!(detect_language(path), Some("rust"));
    }

    #[test]
    fn test_looks_like_email() {
        assert!(looks_like_email("user@example.com"));
        assert!(!looks_like_email("not-an-email"));
    }
}
