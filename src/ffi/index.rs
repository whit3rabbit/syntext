//! `extern "C"` entry points for the native directory index (projects).

use std::os::raw::c_char;
use std::path::PathBuf;

use crate::ffi::dto::{
    from_file_matches, ConfigJson, LimitsJson, MatchDto, SearchFreshDto, SearchOptionsJson,
    StatsDto, UpdateOutcomeDto,
};
use crate::ffi::{
    borrow_index, catch, ffi_ptr, ffi_status, owned_json, parse_opt_json, req_str, syntext_error,
    syntext_index, DEFAULT_LIMITS,
};

/// Build an index under `index_dir` by walking `repo_root` (gitignore-aware).
///
/// `config_json` (nullable): `ConfigJson` per docs/SWIFT.md; NULL/"{}" uses
/// defaults. Returns an owned handle; release with `syntext_index_free`.
#[no_mangle]
pub extern "C" fn syntext_index_build(
    index_dir: *const c_char,
    repo_root: *const c_char,
    config_json: *const c_char,
    err_out: *mut *mut syntext_error,
) -> *mut syntext_index {
    ffi_ptr(err_out, || unsafe {
        let index_dir = req_str(index_dir, "index_dir")?;
        let repo_root = req_str(repo_root, "repo_root")?;
        let config = parse_opt_json::<ConfigJson>(config_json, "config")?
            .into_config(PathBuf::from(index_dir), PathBuf::from(repo_root));
        let index = crate::index::Index::build(config)?;
        Ok(Box::into_raw(Box::new(index)) as *mut syntext_index)
    })
}

/// Open an existing index from `index_dir` (shared flock). Returns an owned
/// handle; release with `syntext_index_free`.
#[no_mangle]
pub extern "C" fn syntext_index_open(
    index_dir: *const c_char,
    repo_root: *const c_char,
    config_json: *const c_char,
    err_out: *mut *mut syntext_error,
) -> *mut syntext_index {
    ffi_ptr(err_out, || unsafe {
        let index_dir = req_str(index_dir, "index_dir")?;
        let repo_root = req_str(repo_root, "repo_root")?;
        let config = parse_opt_json::<ConfigJson>(config_json, "config")?
            .into_config(PathBuf::from(index_dir), PathBuf::from(repo_root));
        let index = crate::index::Index::open(config)?;
        Ok(Box::into_raw(Box::new(index)) as *mut syntext_index)
    })
}

/// Release an index handle. NULL-safe.
#[no_mangle]
pub extern "C" fn syntext_index_free(idx: *mut syntext_index) {
    // Dropping the boxed Index runs arbitrary destructor code (e.g. releasing
    // an flock); route it through the same panic firewall as every other
    // entry point so a panicking Drop cannot unwind across the FFI boundary.
    let _ = catch(|| {
        if !idx.is_null() {
            drop(unsafe { Box::from_raw(idx as *mut crate::index::Index) });
        }
        Ok(())
    });
}

/// Search for `pattern` (literal or regex). `options_json` (nullable):
/// `SearchOptionsJson`. Returns an owned JSON array of `MatchDto`.
#[no_mangle]
pub extern "C" fn syntext_index_search(
    idx: *const syntext_index,
    pattern: *const c_char,
    options_json: *const c_char,
    err_out: *mut *mut syntext_error,
) -> *mut c_char {
    ffi_ptr(err_out, || unsafe {
        let idx = borrow_index(idx)?;
        let pattern = req_str(pattern, "pattern")?;
        let opts_json = parse_opt_json::<SearchOptionsJson>(options_json, "options")?;
        let before = opts_json.before_context.unwrap_or(0);
        let after = opts_json.after_context.unwrap_or(0);
        let (routing_pattern, opts) = opts_json.into_search_options_and_effective_pattern(pattern);

        let dtos = if before > 0 || after > 0 {
            let groups = idx.search_grouped(&routing_pattern, &opts)?;
            from_file_matches(&groups, before, after)
        } else {
            let matches = idx.search(&routing_pattern, &opts)?;
            matches.iter().map(MatchDto::from_search_match).collect()
        };
        owned_json(&dtos)
    })
}

/// Bounded git auto-update, then search. `limits_json` (nullable):
/// `LimitsJson`; NULL uses the CLI defaults (200 files / 150 ms); `{}` means
/// no limits. Returns `{"matches": [...], "update_outcome": {...}}`.
#[no_mangle]
pub extern "C" fn syntext_index_search_fresh(
    idx: *const syntext_index,
    pattern: *const c_char,
    options_json: *const c_char,
    limits_json: *const c_char,
    err_out: *mut *mut syntext_error,
) -> *mut c_char {
    ffi_ptr(err_out, || unsafe {
        let idx = borrow_index(idx)?;
        let pattern = req_str(pattern, "pattern")?;
        let opts_json = parse_opt_json::<SearchOptionsJson>(options_json, "options")?;
        let before = opts_json.before_context.unwrap_or(0);
        let after = opts_json.after_context.unwrap_or(0);
        let (routing_pattern, opts) = opts_json.into_search_options_and_effective_pattern(pattern);
        let limits = if limits_json.is_null() {
            DEFAULT_LIMITS
        } else {
            parse_opt_json::<LimitsJson>(limits_json, "limits")?.into_limits()
        };

        let (dtos, outcome) = if before > 0 || after > 0 {
            let (groups, outcome) = idx.search_grouped_fresh(&routing_pattern, &opts, limits)?;
            (from_file_matches(&groups, before, after), outcome)
        } else {
            let (matches, outcome) = idx.search_fresh(&routing_pattern, &opts, limits)?;
            let dtos = matches.iter().map(MatchDto::from_search_match).collect();
            (dtos, outcome)
        };
        let dto = SearchFreshDto {
            matches: dtos,
            update_outcome: UpdateOutcomeDto::from(outcome),
        };
        owned_json(&dto)
    })
}

/// Index statistics as `StatsDto` JSON.
#[no_mangle]
pub extern "C" fn syntext_index_stats(
    idx: *const syntext_index,
    err_out: *mut *mut syntext_error,
) -> *mut c_char {
    ffi_ptr(err_out, || unsafe {
        let idx = borrow_index(idx)?;
        owned_json(&StatsDto::from(idx.stats()))
    })
}

/// Run bounded git change detection and apply it to the index. `limits_json`
/// (nullable): `LimitsJson` (NULL = CLI defaults, `{}` = no limits). Returns
/// `UpdateOutcomeDto` JSON. Requires `git` on PATH.
#[no_mangle]
pub extern "C" fn syntext_index_update_from_git(
    idx: *const syntext_index,
    limits_json: *const c_char,
    err_out: *mut *mut syntext_error,
) -> *mut c_char {
    ffi_ptr(err_out, || unsafe {
        let idx = borrow_index(idx)?;
        let limits = if limits_json.is_null() {
            DEFAULT_LIMITS
        } else {
            parse_opt_json::<LimitsJson>(limits_json, "limits")?.into_limits()
        };
        let outcome = idx.update_from_git(limits)?;
        owned_json(&UpdateOutcomeDto::from(outcome))
    })
}

/// Buffer a file change. `path` must be absolute, under the repo root (it is
/// resolved by stripping the repo root as a prefix; a bare relative path is
/// rejected as `PathOutsideRepo`). Visible to searches only after
/// `syntext_index_commit_batch`.
#[no_mangle]
pub extern "C" fn syntext_index_notify_change(
    idx: *const syntext_index,
    path: *const c_char,
    err_out: *mut *mut syntext_error,
) -> i32 {
    ffi_status(err_out, || unsafe {
        let idx = borrow_index(idx)?;
        let path = req_str(path, "path")?;
        idx.notify_change(std::path::Path::new(path))?;
        Ok(())
    })
}

/// Buffer a file deletion. Same path contract as `syntext_index_notify_change`
/// (absolute, under the repo root). Visible to searches only after
/// `syntext_index_commit_batch`.
#[no_mangle]
pub extern "C" fn syntext_index_notify_delete(
    idx: *const syntext_index,
    path: *const c_char,
    err_out: *mut *mut syntext_error,
) -> i32 {
    ffi_status(err_out, || unsafe {
        let idx = borrow_index(idx)?;
        let path = req_str(path, "path")?;
        idx.notify_delete(std::path::Path::new(path))?;
        Ok(())
    })
}

/// Apply all buffered notifications atomically (snapshot swap).
#[no_mangle]
pub extern "C" fn syntext_index_commit_batch(
    idx: *const syntext_index,
    err_out: *mut *mut syntext_error,
) -> i32 {
    ffi_status(err_out, || unsafe {
        let idx = borrow_index(idx)?;
        idx.commit_batch()?;
        Ok(())
    })
}

/// Full checksum verification of all base segments (O(index) I/O).
#[no_mangle]
pub extern "C" fn syntext_index_verify(
    idx: *const syntext_index,
    err_out: *mut *mut syntext_error,
) -> i32 {
    ffi_status(err_out, || unsafe {
        let idx = borrow_index(idx)?;
        idx.verify()?;
        Ok(())
    })
}
