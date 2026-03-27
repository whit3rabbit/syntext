use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use ripline_rs::index::Index;
use ripline_rs::Config;

pub fn create_synthetic_repo(file_count: usize) -> TempDir {
    let repo = tempfile::tempdir().unwrap();
    let rust_dir = repo.path().join("src/rust");
    let py_dir = repo.path().join("src/python");
    let ts_dir = repo.path().join("src/typescript");
    fs::create_dir_all(&rust_dir).unwrap();
    fs::create_dir_all(&py_dir).unwrap();
    fs::create_dir_all(&ts_dir).unwrap();

    for i in 0..file_count {
        let (dir, ext) = match i % 3 {
            0 => (&rust_dir, "rs"),
            1 => (&py_dir, "py"),
            _ => (&ts_dir, "ts"),
        };
        let path = dir.join(format!("module_{i:04}.{ext}"));
        fs::write(&path, synthetic_file_contents(i, ext)).unwrap();
    }

    repo
}

pub fn build_index_for_repo(repo_root: &Path) -> (TempDir, Index) {
    let index_dir = tempfile::tempdir().unwrap();
    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo_root.to_path_buf(),
        ..Config::default()
    };
    let index = Index::build(config).unwrap();
    (index_dir, index)
}

#[allow(dead_code)]
pub fn mutable_bench_setup(file_count: usize) -> (TempDir, TempDir, Index, PathBuf) {
    let repo = create_synthetic_repo(file_count);
    let (index_dir, index) = build_index_for_repo(repo.path());
    let target = repo.path().join("src/rust/module_0000.rs");
    (repo, index_dir, index, target)
}

fn synthetic_file_contents(i: usize, ext: &str) -> String {
    let rare_fn = if i.is_multiple_of(25) {
        "fn fn_parse_filter_query(input: &str) -> Result<(String, String), String> {\n    Ok((input.to_string(), input.to_string()))\n}\n"
    } else {
        ""
    };

    match ext {
        "rs" => format!(
            "// synthetic rust file {i}\n\
             pub fn parse_query_{i}(input: &str) -> String {{ input.to_string() }}\n\
             pub fn process_batch_{i}(items: &[String]) -> usize {{ items.len() }}\n\
             pub fn detect_language_{i}(path: &str) -> &'static str {{ if path.ends_with(\".rs\") {{ \"rust\" }} else {{ \"other\" }} }}\n\
             {rare_fn}"
        ),
        "py" => format!(
            "# synthetic python file {i}\n\
             def parse_query_{i}(input):\n    return input\n\
             def process_batch_{i}(items):\n    return len(items)\n\
             def detect_language_{i}(path):\n    return 'python' if path.endswith('.py') else 'other'\n"
        ),
        _ => format!(
            "// synthetic typescript file {i}\n\
             export function parse_query_{i}(input: string): string {{ return input; }}\n\
             export function process_batch_{i}(items: string[]): number {{ return items.length; }}\n\
             export function detect_language_{i}(path: string): string {{ return path.endsWith('.ts') ? 'typescript' : 'other'; }}\n"
        ),
    }
}
