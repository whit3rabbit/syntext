#![allow(dead_code)]

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;
use serde_json::Value;
use tempfile::TempDir;
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// Base64 Decoder (dependency-free)
// ---------------------------------------------------------------------------

pub fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut map = [u8::MAX; 256];
    for (i, &c) in TABLE.iter().enumerate() {
        map[c as usize] = i as u8;
    }
    
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\r' || bytes[i] == b'\n' || bytes[i] == b' ' {
            i += 1;
            continue;
        }
        if i + 4 > bytes.len() {
            return Err("truncated base64".to_string());
        }
        let chunk = &bytes[i..i+4];
        let mut val = 0u32;
        let mut padding = 0;
        for (j, &c) in chunk.iter().enumerate().take(4) {
            if c == b'=' {
                padding += 1;
            } else {
                let idx = map[c as usize];
                if idx == u8::MAX {
                    return Err(format!("invalid base64 char: {}", c as char));
                }
                val |= (idx as u32) << (18 - j * 6);
            }
        }
        out.push((val >> 16) as u8);
        if padding < 2 {
            out.push(((val >> 8) & 0xFF) as u8);
        }
        if padding < 1 {
            out.push((val & 0xFF) as u8);
        }
        i += 4;
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Canonical Match Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CanonicalSubmatch {
    pub start: usize,
    pub end: usize,
    pub text: Vec<u8>,
}

/// A normalized match for Tier A/B comparison.
///
/// Deliberately excludes the matched line's text. st's `normalize_encoding`
/// converts UTF-16/BOM/non-UTF-8 content to UTF-8 at index time, while rg
/// decodes the same bytes and renders invalid sequences as U+FFFD, so the two
/// render non-UTF-8 lines differently even when they agree on the match. Per
/// DIVERGENCES.md, Tier B is `(path, line_number, submatch_start, submatch_end)`
/// only; line-text rendering is a Tier C concern, checked separately via
/// `compare_rendered_output` on raw stdout.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CanonicalMatch {
    pub path: String,
    pub line_number: usize,
    pub submatches: Vec<CanonicalSubmatch>,
}

// ---------------------------------------------------------------------------
// NDJSON Normalizer
// ---------------------------------------------------------------------------

pub fn normalize_ndjson(stdout: &[u8]) -> Result<BTreeSet<CanonicalMatch>, String> {
    let mut matches = BTreeSet::new();
    for line in stdout.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let val: Value = serde_json::from_slice(line)
            .map_err(|e| format!("invalid JSON line: {}, err: {}", String::from_utf8_lossy(line), e))?;
        
        if val.get("type").and_then(|v| v.as_str()) == Some("match") {
            let data = val.get("data").ok_or("match record missing data")?;
            let path_val = data.get("path").ok_or("match record missing path")?;
            let path_bytes = extract_data_as_bytes(path_val)?;
            let path_str = String::from_utf8_lossy(&path_bytes).into_owned();
            let mut path_normalized = path_str.replace('\\', "/");
            if path_normalized.starts_with("./") {
                path_normalized = path_normalized[2..].to_string();
            }
            
            let line_num = data.get("line_number")
                .and_then(|v| v.as_u64())
                .ok_or("match record missing or invalid line_number")? as usize;

            // Line text is intentionally NOT captured for Tier A/B comparison
            // (see the CanonicalMatch doc): st and rg render non-UTF-8 lines
            // differently even when they agree on the match.
            let submatches_arr = data.get("submatches")
                .and_then(|v| v.as_array())
                .ok_or("match record missing or invalid submatches")?;
                
            let mut submatches = Vec::new();
            for sub in submatches_arr {
                let start = sub.get("start")
                    .and_then(|v| v.as_u64())
                    .ok_or("submatch missing or invalid start")? as usize;
                let end = sub.get("end")
                    .and_then(|v| v.as_u64())
                    .ok_or("submatch missing or invalid end")? as usize;
                let m_val = sub.get("match").ok_or("submatch missing match")?;
                let m_bytes = extract_data_as_bytes(m_val)?;
                submatches.push(CanonicalSubmatch {
                    start,
                    end,
                    text: m_bytes,
                });
            }
            submatches.sort();
            
            matches.insert(CanonicalMatch {
                path: path_normalized,
                line_number: line_num,
                submatches,
            });
        }
    }
    Ok(matches)
}

fn extract_data_as_bytes(val: &Value) -> Result<Vec<u8>, String> {
    if let Some(text) = val.get("text").and_then(|v| v.as_str()) {
        Ok(text.as_bytes().to_vec())
    } else if let Some(bytes_b64) = val.get("bytes").and_then(|v| v.as_str()) {
        base64_decode(bytes_b64)
    } else {
        Err(format!("invalid data field: {:?}", val))
    }
}

// ---------------------------------------------------------------------------
// Subprocess Runner
// ---------------------------------------------------------------------------

pub fn run_differential(
    corpus: &[(String, Vec<u8>)],
    query: &str,
    flags: &[&str],
) -> Result<(), String> {
    run_differential_with_tier_c(corpus, query, flags)
}

pub fn rg_available() -> bool {
    Command::new("rg").arg("--version").output().is_ok()
}

// ---------------------------------------------------------------------------
// Pre-Mutation Tree Snapshot (staleness invariant pair)
// ---------------------------------------------------------------------------

/// Recursively copy `repo`'s working tree into a new temporary directory,
/// skipping `.git` (version-control metadata) and `.syntext` (the index
/// directory, when co-located under the repo root).
///
/// This captures the *pre-mutation* state of the tree so the "stale half" of
/// the staleness-invariant pair (see `oracle_freshness.rs`) can run `rg`
/// against the old file contents after a mutation has already been applied
/// to `repo` on disk. Without this, there is no way to ask "what did `rg` see
/// before the change?" once the live working tree has moved on -- a second
/// `git checkout`/stash against the same live tree would race with the
/// mutation under test and is not needed when a plain filesystem copy
/// suffices.
///
/// Symlinks are intentionally skipped (not followed, not copied as links):
/// the oracle corpora used here never rely on symlinks, and copying a
/// symlink's target verbatim risks resolving outside the snapshot directory.
pub fn snapshot_tree(repo: &Path) -> TempDir {
    let snapshot = TempDir::new().expect("failed to create snapshot temp dir");
    copy_dir_recursive(repo, snapshot.path());
    snapshot
}

fn copy_dir_recursive(src: &Path, dst: &Path) {
    let entries = std::fs::read_dir(src)
        .unwrap_or_else(|e| panic!("failed to read dir {:?} for snapshot: {}", src, e));
    for entry in entries {
        let entry = entry.expect("failed to read dir entry for snapshot");
        let file_name = entry.file_name();
        if file_name == ".git" || file_name == ".syntext" {
            continue;
        }
        let src_path = entry.path();
        let dst_path = dst.join(&file_name);
        let file_type = entry
            .file_type()
            .expect("failed to get file type for snapshot");
        if file_type.is_dir() {
            std::fs::create_dir_all(&dst_path).expect("failed to create snapshot subdir");
            copy_dir_recursive(&src_path, &dst_path);
        } else if file_type.is_file() {
            std::fs::copy(&src_path, &dst_path).expect("failed to copy file for snapshot");
        }
        // Symlinks (file_type.is_symlink()) are deliberately skipped; see the
        // doc comment on `snapshot_tree`.
    }
}

fn run_cmd(cwd: &std::path::Path, bin: &str, args: &[&str]) -> Result<(), String> {
    let out = Command::new(bin)
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("failed to run {}: {}", bin, e))?;
    if !out.status.success() {
        return Err(format!(
            "{} {:?} failed in {:?}: stdout={}, stderr={}",
            bin,
            args,
            cwd,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Strategies & Generators (Phase 3)
// ---------------------------------------------------------------------------

fn generate_file_content() -> impl Strategy<Value = Vec<u8>> {
    let word_strategy = prop_oneof![
        Just(b"fn".to_vec()),
        Just(b"let".to_vec()),
        Just(b"def".to_vec()),
        Just(b"parse".to_vec()),
        Just(b"query".to_vec()),
        Just(b"reparse".to_vec()),
        Just(b"camelCase".to_vec()),
        Just(b"snake_case".to_vec()),
        Just(b"a".repeat(129).to_vec()), // long token
    ];
    let sep_strategy = prop_oneof![
        Just(b" ".to_vec()),
        Just(b"\n".to_vec()),
        Just(b"\r\n".to_vec()),
        Just(b"(".to_vec()),
        Just(b")".to_vec()),
        Just(b";".to_vec()),
    ];
    
    let text_strategy = prop::collection::vec((word_strategy, sep_strategy), 5..30)
        .prop_map(|pairs| {
            let mut buf = Vec::new();
            for (w, s) in pairs {
                buf.extend(w);
                buf.extend(s);
            }
            buf
        });
        
    prop_oneof![
        text_strategy.clone(),
        Just(Vec::new()),
        Just(vec![b'a']),
        Just(vec![b'f', b'o', b'o', 0xFF, 0xFE, b'b', b'a', b'r']), // invalid UTF-8
        text_strategy.clone().prop_map(|mut bytes| {
            let mut bom = vec![0xEF, 0xBB, 0xBF];
            bom.append(&mut bytes);
            bom
        }),
        text_strategy.clone().prop_map(|bytes| {
            let mut out = vec![0xFF, 0xFE];
            for &b in &bytes {
                out.push(b);
                out.push(0);
            }
            out
        }),
        text_strategy.clone().prop_map(|bytes| {
            let mut out = vec![0xFE, 0xFF];
            for &b in &bytes {
                out.push(0);
                out.push(b);
            }
            out
        }),
        text_strategy.clone().prop_map(|bytes| {
            let mut out = vec![b'a'; 8191];
            out.push(0); // binary
            out.extend(&bytes);
            out
        }),
    ]
}

pub fn generate_corpus() -> impl Strategy<Value = Vec<(String, Vec<u8>)>> {
    let file_strategy = (
        prop_oneof![
            Just("src/main.rs".to_string()),
            Just("src/lib.rs".to_string()),
            Just("src/nested/helper.rs".to_string()),
            Just("docs/README.md".to_string()),
            Just("main.py".to_string()),
            Just("invalid_utf8.txt".to_string()),
        ],
        generate_file_content(),
    );
    prop::collection::vec(file_strategy, 1..=4).prop_map(|files| {
        let mut seen = std::collections::HashSet::new();
        let mut deduped = Vec::new();
        for (path, content) in files {
            if seen.insert(path.clone()) {
                deduped.push((path, content));
            }
        }
        deduped
    })
}

pub fn generate_query(corpus: &[(String, Vec<u8>)]) -> impl Strategy<Value = String> {
    let mut candidates = Vec::new();
    
    for (_, content) in corpus {
        if !content.is_empty() {
            let s = String::from_utf8_lossy(content);
            let chars: Vec<char> = s.chars().collect();
            if chars.len() >= 5 {
                for i in 0..(chars.len() - 5).min(10) {
                    let sub: String = chars[i..i+5].iter().collect();
                    let cleaned: String = sub.chars().filter(|&c| {
                        !matches!(c, '.' | '*' | '+' | '?' | '[' | ']' | '{' | '}' | '(' | ')' | '|' | '^' | '$' | '\\' | '\r' | '\n' | '\0')
                    }).collect();
                    if !cleaned.trim().is_empty() {
                        candidates.push(cleaned);
                    }
                }
            }
        }
    }
    
    if candidates.is_empty() {
        candidates.push("parse".to_string());
    }
    
    let candidates_len = candidates.len();
    let candidates_clone = candidates.clone();
    let index_strategy = 0..candidates_len;
    let corpus_sub_strategy = index_strategy.prop_map(move |idx| candidates_clone[idx].clone());

    prop_oneof![
        corpus_sub_strategy,
        Just("let".to_string()),
        Just("parse_query".to_string()),
        Just("_parse_query_".to_string()),
        Just("def".to_string()),
        Just("(fn)?let".to_string()),
        Just("(def)?parse".to_string()),
        Just("fn|let|def|parse|query".to_string()),
        Just("a|b|c|d|e|f|g|h|i|j|k|l|m|n|o|p|q|r|s|t|u".to_string()),
        Just("parse_quer[yi]".to_string()),
        // `(?-u)\xff\xfe` (raw byte pattern) is intentionally NOT sampled: rg
        // applies a word-boundary quirk for byte patterns on invalid-UTF-8
        // content (`rg -w '(?-u)\xff\xfe'` on b"foo\xff\xfebar" reports no
        // match) that st's regex-crate `\b` does not, so the two disagree only
        // under `-w`. Documented in tests/oracle/DIVERGENCES.md #11. Invalid
        // -UTF-8 files stay in the corpus and are still covered via the
        // token-aligned queries above.
    ]
}

// ---------------------------------------------------------------------------
// Phase 4: Flag Classification & Sampling
// ---------------------------------------------------------------------------

/// Comparable flags: both st and rg implement them and results are directly
/// comparable via Tier A+B. Some also have Tier C (rendered-output) coverage.
pub const FLAGS_COMPARABLE: &[&str] = &[
    "-i",
    "-w",
    "-x",
    "-F",
    "-c",
    "-l",
    "-o",
    "--vimgrep",
    "--column",
    "--byte-offset",
    "--trim",
];

/// Divergent flags: st implements them but with documented semantic differences
/// from rg (see DIVERGENCES.md). Only Tier A checking is applied.
pub const FLAGS_DIVERGENT: &[&str] = &["-v", "--invert-match", "-S", "--smart-case"];

/// Flags that are output-format selectors — only one may be active at a time.
const OUTPUT_FORMAT_FLAGS: &[&str] = &["-c", "-l", "-o", "--vimgrep", "--json"];

/// Flags incompatible with context lines (-A/-B/-C).
const NO_CONTEXT_FLAGS: &[&str] = &["-c", "-l", "-o", "--vimgrep"];

/// Generate 0–3 compatible flags for a differential test run.
///
/// Compatibility rules enforced:
/// - At most one output format flag (`-c`, `-l`, `-o`, `--vimgrep`).
/// - `-w` and `-x` are mutually exclusive.
/// - Context flags (`-A`/`-B`/`-C`) are excluded when an output-format flag is present.
/// - `-F` is excluded when the query contains regex metacharacters (caller responsibility).
pub fn generate_flags() -> impl Strategy<Value = Vec<&'static str>> {
    // Build a pool of "safe" comparable flags (no output-format conflicts resolved here;
    // we pick from the pool and deduplicate after).
    let pool: &[&str] = &["-i", "-w", "-x", "--column", "--byte-offset", "--trim"];
    let output_pool: &[&str] = &["-c", "-l", "-o", "--vimgrep"];

    // Independently sample: 0–1 output-format flag, 0–2 other flags.
    let output_strat = prop_oneof![
        4 => Just(None::<&'static str>),
        1 => prop::sample::select(output_pool).prop_map(Some),
    ];

    let other_strat = prop::collection::vec(
        prop::sample::select(pool),
        0..=2,
    );

    (output_strat, other_strat).prop_map(|(output_flag, mut other_flags)| {
        // Remove -w if -x is already present and vice versa
        let has_x = other_flags.contains(&"-x");
        let has_w = other_flags.contains(&"-w");
        if has_x && has_w {
            other_flags.retain(|&f| f != "-w");
        }

        // Remove context-incompatible flags when output format selected
        if output_flag.map_or(false, |f| NO_CONTEXT_FLAGS.contains(&f)) {
            other_flags.retain(|&f| !matches!(f, "-A" | "-B" | "-C"));
        }

        // --byte-offset and --column add extra fields to Tier C rendered output
        // (e.g. vimgrep, -c, -l) in ways that differ between st and rg, so exclude
        // them when any Tier C format flag is active.
        if output_flag.is_some() {
            other_flags.retain(|&f| f != "--byte-offset" && f != "--column");
        }

        // Deduplicate
        let mut seen = std::collections::HashSet::new();
        let mut result: Vec<&'static str> = Vec::new();
        for f in other_flags {
            if seen.insert(f) {
                result.push(f);
            }
        }
        if let Some(of) = output_flag {
            result.push(of);
        }
        result
    })
}

// ---------------------------------------------------------------------------
// Phase 4.3: Tier C Rendered-Output Comparison
// ---------------------------------------------------------------------------

/// Tier C comparison mode — determines which rendered-output format to verify.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TierC {
    /// `--vimgrep`: normalize `./` paths, strip column field, sort lines.
    Vimgrep,
    /// `-c` or `--count-matches`: normalize `./` paths, sort lines.
    Count,
    /// `-l` (`--files-with-matches`): normalize `./` paths, sort lines.
    FilesList,
}

impl TierC {
    /// Detect which Tier C mode applies (if any) given a flag list.
    pub fn detect(flags: &[&str]) -> Option<Self> {
        for &f in flags {
            match f {
                "--vimgrep" => return Some(TierC::Vimgrep),
                "-c" | "--count-matches" => return Some(TierC::Count),
                "-l" | "--files-with-matches" => return Some(TierC::FilesList),
                _ => {}
            }
        }
        None
    }
}

/// Compare rendered output between `st` and `rg` for the subset of flags
/// where byte-identical output (after normalization) is the contract.
///
/// Returns `Ok(())` if the outputs agree, or `Err(msg)` with a diff summary.
pub fn compare_rendered_output(
    tier: TierC,
    st_stdout: &[u8],
    rg_stdout: &[u8],
) -> Result<(), String> {
    let normalize_line = |line: &str| -> String {
        // Strip leading "./" from paths
        let line = if line.starts_with("./") { &line[2..] } else { line };
        // Strip trailing \r
        let line = line.trim_end_matches('\r');
        match tier {
            TierC::Vimgrep => {
                // vimgrep format: path:line:col:content
                // Strip the column field (3rd colon-delimited token).
                let parts: Vec<&str> = line.splitn(4, ':').collect();
                if parts.len() == 4 {
                    // parts[0]=path, parts[1]=line, parts[2]=col (skip), parts[3]=content
                    format!("{}:{}:{}", parts[0], parts[1], parts[3])
                } else {
                    line.to_string()
                }
            }
            TierC::Count | TierC::FilesList => line.to_string(),
        }
    };

    let to_sorted_set = |raw: &[u8]| -> std::collections::BTreeSet<String> {
        String::from_utf8_lossy(raw)
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(normalize_line)
            .collect()
    };

    let st_set = to_sorted_set(st_stdout);
    let rg_set = to_sorted_set(rg_stdout);

    if st_set != rg_set {
        let mut only_st: Vec<&String> = st_set.difference(&rg_set).collect();
        let mut only_rg: Vec<&String> = rg_set.difference(&st_set).collect();
        only_st.sort();
        only_rg.sort();
        return Err(format!(
            "Tier C Violation ({:?}): rendered output differs.\n\
             Lines only in st:\n{}\n\
             Lines only in rg:\n{}",
            tier,
            only_st.iter().map(|s| format!("  {s}")).collect::<Vec<_>>().join("\n"),
            only_rg.iter().map(|s| format!("  {s}")).collect::<Vec<_>>().join("\n"),
        ));
    }
    Ok(())
}

/// Extended differential runner that also applies Tier C comparison when
/// the flag list contains a Tier C-comparable output format.
pub fn run_differential_with_tier_c_raw(
    corpus: &[(String, Vec<u8>)],
    query: &str,
    flags: &[&str],
) -> Result<(), String> {
    if !rg_available() {
        return Ok(());
    }

    let temp = TempDir::new().map_err(|e| format!("failed to create temp dir: {}", e))?;

    for (rel_path, content) in corpus {
        let abs_path = temp.path().join(rel_path);
        if let Some(parent) = abs_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create dir {:?}: {}", parent, e))?;
        }
        std::fs::write(&abs_path, content)
            .map_err(|e| format!("failed to write file {:?}: {}", abs_path, e))?;
    }

    run_cmd(temp.path(), "git", &["init"])?;
    run_cmd(temp.path(), "git", &["config", "user.name", "oracle"])?;
    run_cmd(temp.path(), "git", &["config", "user.email", "oracle@example.com"])?;

    // .gitignore must be committed so both st (git-walk) and rg (ignore logic) respect it.
    std::fs::write(temp.path().join(".gitignore"), b".syntext/\n.git/\n")
        .map_err(|e| format!("failed to write gitignore: {}", e))?;

    run_cmd(temp.path(), "git", &["add", "."])?;
    run_cmd(temp.path(), "git", &["commit", "-m", "initial", "--no-gpg-sign"])?;

    let st_bin = env!("CARGO_BIN_EXE_st");
    let index_dir = temp.path().join(".syntext");
    run_cmd(
        temp.path(),
        st_bin,
        &["--repo-root", temp.path().to_str().unwrap(), "--index-dir", index_dir.to_str().unwrap(), "index"],
    )?;

    let mut st_args = vec![
        "--repo-root", temp.path().to_str().unwrap(),
        "--index-dir", index_dir.to_str().unwrap(),
    ];
    // Add output format flags from caller; default to --json unless a Tier C flag is present
    let tier = TierC::detect(flags);
    if tier.is_none() {
        st_args.push("--json");
    }
    st_args.extend(flags.iter().copied());
    st_args.push(query);

    let st_output = Command::new(st_bin)
        .args(&st_args)
        .current_dir(temp.path())
        .env("SYNTEXT_DETERMINISTIC", "1")
        .output()
        .map_err(|e| format!("failed to run st: {}", e))?;

    let mut rg_args = vec![
        "--hidden",
        "--crlf",
    ];
    if tier.is_none() {
        rg_args.push("--json");
    }
    rg_args.extend(flags.iter().copied());
    // If -e flags are present, rg already has the pattern; positional query would be a path.
    let has_e_flag = flags.iter().any(|&f| f == "-e");
    if !has_e_flag {
        rg_args.push(query);
    }
    rg_args.push(".");

    let rg_output = Command::new("rg")
        .args(&rg_args)
        .current_dir(temp.path())
        .output()
        .map_err(|e| format!("failed to run rg: {}", e))?;

    let st_code = st_output.status.code().unwrap_or(-1);
    let rg_code = rg_output.status.code().unwrap_or(-1);

    let st_norm = if st_code == 0 { 0 } else if st_code == 1 { 1 } else { 2 };
    let rg_norm = if rg_code == 0 { 0 } else if rg_code == 1 { 1 } else { 2 };

    if st_norm != rg_norm {
        return Err(format!(
            "Exit code mismatch (normalised): st={} (raw={}), rg={} (raw={}).\n\
             st stderr: {}\n\
             rg stderr: {}",
            st_norm, st_code,
            rg_norm, rg_code,
            String::from_utf8_lossy(&st_output.stderr),
            String::from_utf8_lossy(&rg_output.stderr),
        ));
    }

    // Tier C: rendered-output byte comparison
    if let Some(tc) = tier {
        return compare_rendered_output(tc, &st_output.stdout, &rg_output.stdout);
    }

    // Tier A + B via NDJSON normalizer
    let has_invert_match = flags.iter().any(|&f| f == "-v" || f == "--invert-match");

    let st_matches = normalize_ndjson(&st_output.stdout)
        .map_err(|e| format!("st NDJSON parse error: {}, stdout: {}", e, String::from_utf8_lossy(&st_output.stdout)))?;
    let rg_matches = normalize_ndjson(&rg_output.stdout)
        .map_err(|e| format!("rg NDJSON parse error: {}, stdout: {}", e, String::from_utf8_lossy(&rg_output.stdout)))?;

    for m in &rg_matches {
        if !st_matches.contains(m) {
            return Err(format!(
                "Tier A Violation (false negative): rg found match {:?}, but st did not.\n\
                 st stdout:\n{}\n\
                 rg stdout:\n{}",
                m,
                String::from_utf8_lossy(&st_output.stdout),
                String::from_utf8_lossy(&rg_output.stdout),
            ));
        }
    }

    if !has_invert_match && st_matches.len() != rg_matches.len() {
        return Err(format!(
            "Tier B Violation (mismatch): st and rg match sets differ in size (st={}, rg={}).\n\
             st matches:\n{:?}\n\
             rg matches:\n{:?}\n\
             st stdout:\n{}\n\
             rg stdout:\n{}",
            st_matches.len(),
            rg_matches.len(),
            st_matches,
            rg_matches,
            String::from_utf8_lossy(&st_output.stdout),
            String::from_utf8_lossy(&rg_output.stdout),
        ));
    }

    Ok(())
}

pub fn run_differential_with_tier_c(
    corpus: &[(String, Vec<u8>)],
    query: &str,
    flags: &[&str],
) -> Result<(), String> {
    let res = run_differential_with_tier_c_raw(corpus, query, flags);
    if let Err(e) = res {
        eprintln!("ORIGINAL TEST FAILURE DETECTED: {}", e);
        eprintln!("Shrinking failure...");
        let (min_corpus, min_query, min_flags) = shrink_differential(corpus, query, flags);
        
        let min_flags_refs: Vec<&str> = min_flags.iter().map(|s| s.as_str()).collect();
        let final_err = run_differential_with_tier_c_raw(&min_corpus, &min_query, &min_flags_refs)
            .err()
            .unwrap_or_else(|| "No error reproduced after shrinking".to_string());

        if let Err(save_err) = save_regression_fixture(&min_corpus, &min_query, &min_flags) {
            eprintln!("Failed to save regression fixture: {}", save_err);
        }

        let mut corpus_str = String::new();
        for (path, content) in &min_corpus {
            corpus_str.push_str(&format!(
                "        ({:?}.to_string(), {:?}.to_vec()),\n",
                path, content
            ));
        }
        let formatted_repro = format!(
            "================================================================================\n\
             MINIMIZED REPRODUCER:\n\
             --------------------------------------------------------------------------------\n\
             let corpus = vec![\n\
             {}\
             ];\n\
             run_differential_with_tier_c(&corpus, {:?}, &{:?}).unwrap();\n\
             --------------------------------------------------------------------------------\n\
             Error message:\n\
             {}\n\
             ================================================================================",
            corpus_str, min_query, min_flags, final_err
        );
        
        return Err(formatted_repro);
    }
    Ok(())
}

pub fn shrink_differential(
    corpus: &[(String, Vec<u8>)],
    query: &str,
    flags: &[&str],
) -> (Vec<(String, Vec<u8>)>, String, Vec<String>) {
    let mut min_corpus = corpus.to_vec();
    let mut min_query = query.to_string();
    let mut min_flags: Vec<String> = flags.iter().map(|s| s.to_string()).collect();

    let run = |c: &[(String, Vec<u8>)], q: &str, f: &[String]| -> bool {
        let f_refs: Vec<&str> = f.iter().map(|s| s.as_str()).collect();
        run_differential_with_tier_c_raw(c, q, &f_refs).is_err()
    };

    // 1. Minimize flags
    let mut idx = 0;
    while idx < min_flags.len() {
        let mut test_flags = min_flags.clone();
        test_flags.remove(idx);
        if run(&min_corpus, &min_query, &test_flags) {
            min_flags = test_flags;
        } else {
            idx += 1;
        }
    }

    // 2. Minimize files in corpus
    let mut idx = 0;
    while idx < min_corpus.len() {
        let mut test_corpus = min_corpus.clone();
        test_corpus.remove(idx);
        if run(&test_corpus, &min_query, &min_flags) {
            min_corpus = test_corpus;
        } else {
            idx += 1;
        }
    }

    // 3. Minimize content of remaining files
    for file_idx in 0..min_corpus.len() {
        let path = min_corpus[file_idx].0.clone();
        let mut current_content = min_corpus[file_idx].1.clone();

        // 3a. Line-by-line minimization (if UTF-8 text)
        if let Ok(text) = std::str::from_utf8(&current_content) {
            let mut lines: Vec<String> = text.lines().map(|s| s.to_string()).collect();
            let ends_with_newline = text.ends_with('\n');
            let mut line_idx = 0;
            while line_idx < lines.len() {
                let mut test_lines = lines.clone();
                test_lines.remove(line_idx);
                let test_text = test_lines.join("\n") + if ends_with_newline { "\n" } else { "" };
                let test_bytes = test_text.into_bytes();

                let mut test_corpus = min_corpus.clone();
                test_corpus[file_idx] = (path.clone(), test_bytes.clone());

                if run(&test_corpus, &min_query, &min_flags) {
                    lines = test_lines;
                    current_content = test_bytes;
                    min_corpus = test_corpus;
                } else {
                    line_idx += 1;
                }
            }
        }

        // 3b. Byte bisection-style minimization
        let mut chunk_size = current_content.len() / 2;
        while chunk_size > 0 {
            let mut offset = 0;
            while offset + chunk_size <= current_content.len() {
                let mut test_content = current_content.clone();
                test_content.drain(offset..offset + chunk_size);
                
                let mut test_corpus = min_corpus.clone();
                test_corpus[file_idx] = (path.clone(), test_content.clone());

                if run(&test_corpus, &min_query, &min_flags) {
                    current_content = test_content;
                    min_corpus = test_corpus;
                } else {
                    offset += chunk_size;
                }
            }
            chunk_size /= 2;
        }
    }

    // 4. Minimize query string length
    let mut chars: Vec<char> = min_query.chars().collect();
    let mut q_idx = 0;
    while q_idx < chars.len() {
        let mut test_chars = chars.clone();
        test_chars.remove(q_idx);
        let test_query: String = test_chars.iter().collect();
        if !test_query.trim().is_empty() && run(&min_corpus, &test_query, &min_flags) {
            chars = test_chars;
            min_query = test_query;
        } else {
            q_idx += 1;
        }
    }

    (min_corpus, min_query, min_flags)
}

pub fn save_regression_fixture(
    corpus: &[(String, Vec<u8>)],
    query: &str,
    flags: &[String],
) -> Result<(), String> {
    use std::fs;
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use syntext::base64::encode as base64_encode;

    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let regressions_dir = std::path::Path::new(manifest_dir)
        .join("tests")
        .join("oracle")
        .join("regressions");
    
    fs::create_dir_all(&regressions_dir)
        .map_err(|e| format!("failed to create regressions dir: {e}"))?;

    let mut hasher = DefaultHasher::new();
    corpus.hash(&mut hasher);
    query.hash(&mut hasher);
    flags.hash(&mut hasher);
    let hash = hasher.finish();
    let filename = format!("repro_{:016x}.json", hash);
    let filepath = regressions_dir.join(&filename);

    #[derive(serde::Serialize)]
    struct RegressionFixture {
        r#type: String,
        corpus: Vec<FileEntry>,
        query: String,
        flags: Vec<String>,
    }

    #[derive(serde::Serialize)]
    struct FileEntry {
        path: String,
        content_b64: String,
    }

    let file_entries: Vec<FileEntry> = corpus
        .iter()
        .map(|(path, content)| FileEntry {
            path: path.clone(),
            content_b64: base64_encode(content),
        })
        .collect();

    let fixture = RegressionFixture {
        r#type: "cli".to_string(),
        corpus: file_entries,
        query: query.to_string(),
        flags: flags.to_vec(),
    };

    let json_content = serde_json::to_string_pretty(&fixture)
        .map_err(|e| format!("failed to serialize regression fixture: {e}"))?;

    fs::write(&filepath, json_content)
        .map_err(|e| format!("failed to write regression fixture: {e}"))?;

    eprintln!("Successfully saved minimized regression to: {:?}", filepath);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base64_decode() {
        assert_eq!(base64_decode("").unwrap(), b"");
        assert_eq!(base64_decode("Zg==").unwrap(), b"f");
        assert_eq!(base64_decode("Zm8=").unwrap(), b"fo");
        assert_eq!(base64_decode("Zm9v").unwrap(), b"foo");
        assert_eq!(base64_decode("Zm9vYg==").unwrap(), b"foob");
        assert_eq!(base64_decode("Zm9vYmE=").unwrap(), b"fooba");
        assert_eq!(base64_decode("Zm9vYmFy").unwrap(), b"foobar");
        assert_eq!(base64_decode("/wCA").unwrap(), b"\xFF\x00\x80");
    }

    #[test]
    fn snapshot_tree_copies_files_and_skips_git_and_syntext_dirs() {
        let repo = TempDir::new().unwrap();
        std::fs::create_dir_all(repo.path().join("src/nested")).unwrap();
        std::fs::write(repo.path().join("src/main.rs"), b"fn old_marker() {}\n").unwrap();
        std::fs::write(repo.path().join("src/nested/helper.rs"), b"fn helper() {}\n").unwrap();

        // Simulate metadata directories that must NOT be copied into the
        // snapshot: real .git internals and a co-located index dir.
        std::fs::create_dir_all(repo.path().join(".git")).unwrap();
        std::fs::write(repo.path().join(".git/HEAD"), b"ref: refs/heads/main\n").unwrap();
        std::fs::create_dir_all(repo.path().join(".syntext")).unwrap();
        std::fs::write(repo.path().join(".syntext/manifest.json"), b"{}").unwrap();

        let snapshot = snapshot_tree(repo.path());

        assert_eq!(
            std::fs::read(snapshot.path().join("src/main.rs")).unwrap(),
            b"fn old_marker() {}\n"
        );
        assert_eq!(
            std::fs::read(snapshot.path().join("src/nested/helper.rs")).unwrap(),
            b"fn helper() {}\n"
        );
        assert!(
            !snapshot.path().join(".git").exists(),
            ".git must not be copied into the snapshot"
        );
        assert!(
            !snapshot.path().join(".syntext").exists(),
            ".syntext must not be copied into the snapshot"
        );

        // Mutate the ORIGINAL tree after the snapshot was taken: the snapshot
        // must retain the pre-mutation content (that is the entire point of
        // this helper).
        std::fs::write(repo.path().join("src/main.rs"), b"fn new_marker() {}\n").unwrap();
        assert_eq!(
            std::fs::read(snapshot.path().join("src/main.rs")).unwrap(),
            b"fn old_marker() {}\n",
            "snapshot must be unaffected by post-snapshot mutation of the live tree"
        );
    }
}
