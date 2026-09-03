//! Integration tests for the `ffi` C ABI: call the `extern "C"` entry points
//! as ordinary Rust functions and assert the JSON/error contracts documented
//! in `swift/Sources/CSyntext/include/syntext.h` and docs/SWIFT.md.

use std::ffi::{CStr, CString};
use std::fs;
use std::os::raw::c_char;
use std::path::Path;
use std::ptr::{null, null_mut};

use syntext::ffi;

// ── Helpers ─────────────────────────────────────────────────
/// Drain `*err_out` into `(code, message)` and free the handle.
fn take_error(err: *mut ffi::syntext_error) -> Option<(u32, String)> {
    if err.is_null() {
        return None;
    }
    let code = ffi::syntext_error_code(err);
    let msg_ptr = ffi::syntext_error_message(err);
    assert!(!msg_ptr.is_null(), "non-NULL error must carry a message");
    let msg = unsafe { CStr::from_ptr(msg_ptr) }
        .to_string_lossy()
        .into_owned();
    ffi::syntext_error_free(err);
    Some((code, msg))
}

/// Read an owned JSON string and free it.
fn json_from(ptr: *mut c_char) -> String {
    assert!(!ptr.is_null(), "expected an owned JSON string, got NULL");
    let s = unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned();
    ffi::syntext_string_free(ptr);
    s
}

fn expected_rc(err: *mut ffi::syntext_error, want: u32) {
    let (code, msg) = take_error(err).expect("expected an error handle");
    assert_eq!(code, want, "error message: {msg}");
}

fn build_index(idx_dir: &Path, repo: &Path) -> *mut ffi::syntext_index {
    let id = CString::new(idx_dir.to_str().unwrap()).unwrap();
    let rr = CString::new(repo.to_str().unwrap()).unwrap();
    let mut err: *mut ffi::syntext_error = null_mut();
    let idx = ffi::syntext_index_build(id.as_ptr(), rr.as_ptr(), null(), &mut err);
    if idx.is_null() {
        let (code, msg) = take_error(err).expect("build error handle");
        panic!("syntext_index_build failed: {code} {msg}");
    }
    assert!(take_error(err).is_none(), "no error expected on success");
    idx
}

fn open_index(idx_dir: &Path, repo: &Path) -> *mut ffi::syntext_index {
    let id = CString::new(idx_dir.to_str().unwrap()).unwrap();
    let rr = CString::new(repo.to_str().unwrap()).unwrap();
    let mut err: *mut ffi::syntext_error = null_mut();
    let idx = ffi::syntext_index_open(id.as_ptr(), rr.as_ptr(), null(), &mut err);
    if idx.is_null() {
        let (code, msg) = take_error(err).expect("open error handle");
        panic!("syntext_index_open failed: {code} {msg}");
    }
    idx
}

fn search(
    idx: *const ffi::syntext_index,
    pattern: &str,
    options_json: Option<&str>,
) -> serde_json::Value {
    let pat = CString::new(pattern).unwrap();
    let opts = options_json.map(CString::new).map(Result::unwrap);
    let mut err: *mut ffi::syntext_error = null_mut();
    let json = ffi::syntext_index_search(
        idx,
        pat.as_ptr(),
        opts.as_ref().map_or(null(), |s| s.as_ptr()),
        &mut err,
    );
    if json.is_null() {
        let (code, msg) = take_error(err).expect("search error handle");
        panic!("syntext_index_search failed: {code} {msg}");
    }
    serde_json::from_str(&json_from(json)).unwrap()
}

fn fixture_repo(tmp: &Path) -> std::path::PathBuf {
    let repo = tmp.join("repo");
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("a.rs"), b"fn main() { needle_here(); }\n").unwrap();
    fs::write(repo.join("b.txt"), b"nothing to see\n").unwrap();
    repo
}

// ── Shared ──────────────────────────────────────────────────
#[test]
fn version_is_nonempty_static_string() {
    let p = ffi::syntext_version();
    assert!(!p.is_null());
    let s = unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
    assert!(!s.is_empty());
}

#[test]
fn free_functions_are_null_safe() {
    ffi::syntext_string_free(null_mut());
    ffi::syntext_error_free(null_mut());
    ffi::syntext_index_free(null_mut());
    ffi::syntext_mem_index_free(null_mut());
    // error accessors on NULL are defined (UNKNOWN / NULL), not crashes.
    assert_eq!(ffi::syntext_error_code(null()), ffi::SYNTEXT_ERR_UNKNOWN);
    assert!(ffi::syntext_error_message(null()).is_null());
}

// ── Native index ────────────────────────────────────────────
#[test]
fn index_build_search_free_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = fixture_repo(tmp.path());
    let idx_dir = tmp.path().join("idx");
    let idx = build_index(&idx_dir, &repo);

    // Search: exactly one match in a.rs, offsets defined against the b64 bytes.
    let v = search(idx, "needle_here", None);
    let arr = v.as_array().expect("search returns a JSON array");
    assert_eq!(arr.len(), 1);
    let m = &arr[0];
    assert_eq!(m["path"].as_str().unwrap(), "a.rs");
    assert_eq!(m["line_number"].as_u64(), Some(1));
    let line = &b"fn main() { needle_here(); }"[..];
    assert_eq!(m["line_content"].as_str().unwrap().as_bytes(), line);
    assert_eq!(
        m["line_content_b64"].as_str().unwrap(),
        syntext::__internal::encode(line)
    );
    assert_eq!(m["submatch_start"].as_u64(), Some(12));
    assert_eq!(m["submatch_end"].as_u64(), Some(23));
    assert_eq!(m["byte_offset"].as_u64(), Some(12));

    // Stats: fixture files only (temp dir is not a git repo, no .git noise).
    let mut err: *mut ffi::syntext_error = null_mut();
    let stats_json = ffi::syntext_index_stats(idx, &mut err);
    assert!(take_error(err).is_none());
    let stats: serde_json::Value = serde_json::from_str(&json_from(stats_json)).unwrap();
    assert_eq!(stats["total_documents"].as_u64(), Some(2));

    // Verify: freshly built manifest carries a checksum.
    let rc = ffi::syntext_index_verify(idx, &mut err);
    assert_eq!(rc, ffi::SYNTEXT_OK as i32, "{:?}", take_error(err));

    ffi::syntext_index_free(idx);
}

#[test]
fn index_open_after_free_sees_durable_state() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = fixture_repo(tmp.path());
    let idx_dir = tmp.path().join("idx");
    let idx = build_index(&idx_dir, &repo);
    ffi::syntext_index_free(idx);

    let reopened = open_index(&idx_dir, &repo);
    let v = search(reopened, "needle_here", None);
    assert_eq!(v.as_array().unwrap().len(), 1);
    ffi::syntext_index_free(reopened);
}

#[test]
fn notify_change_visible_only_after_commit_batch() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = fixture_repo(tmp.path());
    let idx_dir = tmp.path().join("idx");
    let idx = build_index(&idx_dir, &repo);

    fs::write(repo.join("c.rs"), b"brand_new_needle\n").unwrap();
    let path = CString::new(repo.join("c.rs").to_str().unwrap()).unwrap();
    let mut err: *mut ffi::syntext_error = null_mut();
    let rc = ffi::syntext_index_notify_change(idx, path.as_ptr(), &mut err);
    assert_eq!(rc, ffi::SYNTEXT_OK as i32, "{:?}", take_error(err));

    assert_eq!(
        search(idx, "brand_new_needle", None)
            .as_array()
            .unwrap()
            .len(),
        0
    );

    let rc = ffi::syntext_index_commit_batch(idx, &mut err);
    assert_eq!(rc, ffi::SYNTEXT_OK as i32, "{:?}", take_error(err));
    assert_eq!(
        search(idx, "brand_new_needle", None)
            .as_array()
            .unwrap()
            .len(),
        1
    );

    ffi::syntext_index_free(idx);
}

#[test]
fn search_fresh_envelope_and_no_changes_outcome() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = fixture_repo(tmp.path());
    let idx_dir = tmp.path().join("idx");
    let idx = build_index(&idx_dir, &repo);

    let pat = CString::new("needle_here").unwrap();
    let mut err: *mut ffi::syntext_error = null_mut();
    let json = ffi::syntext_index_search_fresh(idx, pat.as_ptr(), null(), null(), &mut err);
    assert!(!json.is_null(), "{:?}", take_error(err));
    let v: serde_json::Value = serde_json::from_str(&json_from(json)).unwrap();
    assert_eq!(v["matches"].as_array().unwrap().len(), 1);
    // Temp dir is not a git repo: git reports nothing -> NoChanges.
    assert_eq!(v["update_outcome"]["kind"].as_str().unwrap(), "no_changes");

    ffi::syntext_index_free(idx);
}

#[test]
fn update_from_git_on_non_git_dir_is_no_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = fixture_repo(tmp.path());
    let idx_dir = tmp.path().join("idx");
    let idx = build_index(&idx_dir, &repo);

    let mut err: *mut ffi::syntext_error = null_mut();
    let json = ffi::syntext_index_update_from_git(idx, null(), &mut err);
    // git is present on every CI image; if resolution failed, that is an Io
    // error, not a crash — accept both but assert the contract shape.
    if json.is_null() {
        expected_rc(err, ffi::SYNTEXT_ERR_IO);
    } else {
        let v: serde_json::Value = serde_json::from_str(&json_from(json)).unwrap();
        assert_eq!(v["kind"].as_str().unwrap(), "no_changes");
    }

    ffi::syntext_index_free(idx);
}

#[test]
fn error_paths_map_to_stable_codes() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = fixture_repo(tmp.path());
    let idx_dir = tmp.path().join("idx");
    let idx = build_index(&idx_dir, &repo);

    let id = CString::new(tmp.path().join("missing-idx").to_str().unwrap()).unwrap();
    let rr = CString::new(repo.to_str().unwrap()).unwrap();

    // Open on a missing index dir -> INDEX_NOT_FOUND (2).
    let mut err: *mut ffi::syntext_error = null_mut();
    let p = ffi::syntext_index_open(id.as_ptr(), rr.as_ptr(), null(), &mut err);
    assert!(p.is_null());
    expected_rc(err, ffi::SYNTEXT_ERR_INDEX_NOT_FOUND);

    // Invalid regex -> INVALID_PATTERN (3).
    let bad = CString::new("[").unwrap();
    let mut err: *mut ffi::syntext_error = null_mut();
    let p = ffi::syntext_index_search(idx, bad.as_ptr(), null(), &mut err);
    assert!(p.is_null());
    expected_rc(err, ffi::SYNTEXT_ERR_INVALID_PATTERN);

    // Malformed options JSON -> INVALID_ARGUMENT (100).
    let pat = CString::new("needle_here").unwrap();
    let opts = CString::new("{nope").unwrap();
    let mut err: *mut ffi::syntext_error = null_mut();
    let p = ffi::syntext_index_search(idx, pat.as_ptr(), opts.as_ptr(), &mut err);
    assert!(p.is_null());
    expected_rc(err, ffi::SYNTEXT_ERR_INVALID_ARGUMENT);

    // NULL handle -> INVALID_ARGUMENT (100).
    let mut err: *mut ffi::syntext_error = null_mut();
    let p = ffi::syntext_index_search(null(), pat.as_ptr(), null(), &mut err);
    assert!(p.is_null());
    expected_rc(err, ffi::SYNTEXT_ERR_INVALID_ARGUMENT);

    ffi::syntext_index_free(idx);
}

// ── Mem index (chats) ───────────────────────────────────────
fn mem_search(
    midx: *const ffi::syntext_mem_index,
    pattern: &str,
    options_json: Option<&str>,
) -> serde_json::Value {
    let pat = CString::new(pattern).unwrap();
    let opts = options_json.map(CString::new).map(Result::unwrap);
    let mut err: *mut ffi::syntext_error = null_mut();
    let json = ffi::syntext_mem_index_search(
        midx,
        pat.as_ptr(),
        opts.as_ref().map_or(null(), |s| s.as_ptr()),
        &mut err,
    );
    if json.is_null() {
        let (code, msg) = take_error(err).expect("search error handle");
        panic!("syntext_mem_index_search failed: {code} {msg}");
    }
    serde_json::from_str(&json_from(json)).unwrap()
}

fn mem_add(midx: *const ffi::syntext_mem_index, id: &str, content: &[u8]) {
    let id = CString::new(id).unwrap();
    let mut err: *mut ffi::syntext_error = null_mut();
    let rc =
        ffi::syntext_mem_index_add(midx, id.as_ptr(), content.as_ptr(), content.len(), &mut err);
    assert_eq!(rc, ffi::SYNTEXT_OK as i32, "{:?}", take_error(err));
}

fn mem_commit(midx: *const ffi::syntext_mem_index) {
    let mut err: *mut ffi::syntext_error = null_mut();
    let rc = ffi::syntext_mem_index_commit(midx, &mut err);
    assert_eq!(rc, ffi::SYNTEXT_OK as i32, "{:?}", take_error(err));
}

#[test]
fn mem_index_lifecycle_with_non_utf8_content() {
    let mut err: *mut ffi::syntext_error = null_mut();
    let midx = ffi::syntext_mem_index_new(&mut err);
    assert!(!midx.is_null(), "{:?}", take_error(err));

    mem_add(midx, "chats/1", b"chat needle one\n");
    // Invalid UTF-8 without NUL or a BOM prefix (a leading \xFF\xFE would be
    // transcoded as UTF-16LE): indexed; exact bytes must survive the trip.
    mem_add(midx, "chats/2", b"na\xFFve needle text\n");

    // Uncommitted: nothing visible.
    assert_eq!(
        mem_search(midx, "needle", None).as_array().unwrap().len(),
        0
    );
    mem_commit(midx);
    let v = mem_search(midx, "needle", None);
    assert_eq!(v.as_array().unwrap().len(), 2);

    // The invalid-UTF-8 doc round-trips byte-exact via b64; the lossy
    // rendering carries U+FFFD replacement characters instead.
    let m = v
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["path"].as_str().unwrap() == "chats/2")
        .expect("chats/2 match");
    let line = &b"na\xFFve needle text"[..];
    assert_eq!(
        m["line_content_b64"].as_str().unwrap(),
        syntext::__internal::encode(line)
    );
    assert!(m["line_content"].as_str().unwrap().contains('\u{FFFD}'));

    // Remove stops matching only after commit.
    let id = CString::new("chats/2").unwrap();
    let mut err: *mut ffi::syntext_error = null_mut();
    let rc = ffi::syntext_mem_index_remove(midx, id.as_ptr(), &mut err);
    assert_eq!(rc, ffi::SYNTEXT_OK as i32, "{:?}", take_error(err));
    assert_eq!(
        mem_search(midx, "needle", None).as_array().unwrap().len(),
        2
    );
    mem_commit(midx);
    assert_eq!(
        mem_search(midx, "needle", None).as_array().unwrap().len(),
        1
    );

    ffi::syntext_mem_index_free(midx);
}

#[test]
fn mem_index_rejects_traversal_ids() {
    let mut err: *mut ffi::syntext_error = null_mut();
    let midx = ffi::syntext_mem_index_new(&mut err);
    assert!(!midx.is_null(), "{:?}", take_error(err));

    for bad in ["../x", "/abs", "a/../b", ""] {
        let id = CString::new(bad).unwrap();
        let mut err: *mut ffi::syntext_error = null_mut();
        let rc = ffi::syntext_mem_index_add(midx, id.as_ptr(), b"m".as_ptr(), 1, &mut err);
        assert_eq!(rc, ffi::SYNTEXT_ERR_PATH_OUTSIDE_REPO as i32, "id {bad:?}");
        assert!(take_error(err).is_some());
    }

    ffi::syntext_mem_index_free(midx);
}

#[test]
fn mem_index_nul_content_is_accepted_and_skipped_as_binary() {
    let mut err: *mut ffi::syntext_error = null_mut();
    let midx = ffi::syntext_mem_index_new(&mut err);
    assert!(!midx.is_null(), "{:?}", take_error(err));

    // The ABI accepts (ptr, len) bytes containing NUL without erroring; the
    // binary heuristic then skips the document at commit (documented).
    mem_add(midx, "chats/bin", b"needle\x00\x01\x00\x02\x00\x03\x00\x04");
    mem_commit(midx);
    assert_eq!(
        mem_search(midx, "needle", None).as_array().unwrap().len(),
        0
    );

    ffi::syntext_mem_index_free(midx);
}

#[test]
fn mem_index_max_results_default_and_explicit() {
    let mut err: *mut ffi::syntext_error = null_mut();
    let midx = ffi::syntext_mem_index_new(&mut err);
    assert!(!midx.is_null(), "{:?}", take_error(err));

    let doc: Vec<u8> = std::iter::repeat_n(b"hit\n", 12_000)
        .flat_map(|l| l.to_vec())
        .collect();
    mem_add(midx, "big", &doc);
    mem_commit(midx);

    // Absent max_results: FFI default caps at 10_000.
    assert_eq!(
        mem_search(midx, "hit", None).as_array().unwrap().len(),
        10_000
    );
    // Explicit value honored.
    assert_eq!(
        mem_search(midx, "hit", Some(r#"{"max_results": 5}"#))
            .as_array()
            .unwrap()
            .len(),
        5
    );

    ffi::syntext_mem_index_free(midx);
}
