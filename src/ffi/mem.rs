//! `extern "C"` entry points for the mutable in-memory index (chats).

use std::os::raw::c_char;
use std::sync::Arc;

use crate::ffi::dto::{MatchDto, SearchOptionsJson};
use crate::ffi::{
    borrow_mem, bytes, catch, ffi_ptr, ffi_status, owned_json, parse_opt_json, req_str,
    syntext_error, syntext_mem_index,
};

/// Create an empty in-memory document index. Release with
/// `syntext_mem_index_free`.
#[no_mangle]
pub extern "C" fn syntext_mem_index_new(
    err_out: *mut *mut syntext_error,
) -> *mut syntext_mem_index {
    ffi_ptr(err_out, || {
        let idx = crate::index::mem_index::MemIndex::new()?;
        Ok(Box::into_raw(Box::new(idx)) as *mut syntext_mem_index)
    })
}

/// Release a mem-index handle. NULL-safe.
#[no_mangle]
pub extern "C" fn syntext_mem_index_free(midx: *mut syntext_mem_index) {
    // See syntext_index_free: route the drop through the panic firewall.
    let _ = catch(|| {
        if !midx.is_null() {
            drop(unsafe { Box::from_raw(midx as *mut crate::index::mem_index::MemIndex) });
        }
        Ok(())
    });
}

/// Buffer a document. `content` is `(ptr, len)` bytes and may contain NUL and
/// invalid UTF-8. `doc_id` must be a non-empty relative-path-shaped id (no
/// leading `/`, no `..`). Same-id adds replace. Visible to searches only
/// after `syntext_mem_index_commit`.
#[no_mangle]
pub extern "C" fn syntext_mem_index_add(
    midx: *const syntext_mem_index,
    doc_id: *const c_char,
    content: *const u8,
    content_len: usize,
    err_out: *mut *mut syntext_error,
) -> i32 {
    ffi_status(err_out, || unsafe {
        let midx = borrow_mem(midx)?;
        let doc_id = req_str(doc_id, "doc_id")?;
        let content = bytes(content, content_len)?;
        midx.add(doc_id, Arc::from(content))?;
        Ok(())
    })
}

/// Buffer a document deletion (absent id is a no-op). Visible to searches
/// only after `syntext_mem_index_commit`.
#[no_mangle]
pub extern "C" fn syntext_mem_index_remove(
    midx: *const syntext_mem_index,
    doc_id: *const c_char,
    err_out: *mut *mut syntext_error,
) -> i32 {
    ffi_status(err_out, || unsafe {
        let midx = borrow_mem(midx)?;
        let doc_id = req_str(doc_id, "doc_id")?;
        midx.remove(doc_id)?;
        Ok(())
    })
}

/// Rebuild the snapshot from all buffered documents and publish it atomically.
/// O(total content); blocks `add`/`remove` for the duration.
#[no_mangle]
pub extern "C" fn syntext_mem_index_commit(
    midx: *const syntext_mem_index,
    err_out: *mut *mut syntext_error,
) -> i32 {
    ffi_status(err_out, || unsafe {
        let midx = borrow_mem(midx)?;
        midx.commit()?;
        Ok(())
    })
}

/// Search the committed snapshot. `options_json` (nullable):
/// `SearchOptionsJson`. Returns an owned JSON array of `MatchDto`.
#[no_mangle]
pub extern "C" fn syntext_mem_index_search(
    midx: *const syntext_mem_index,
    pattern: *const c_char,
    options_json: *const c_char,
    err_out: *mut *mut syntext_error,
) -> *mut c_char {
    ffi_ptr(err_out, || unsafe {
        let midx = borrow_mem(midx)?;
        let pattern = req_str(pattern, "pattern")?;
        let opts =
            parse_opt_json::<SearchOptionsJson>(options_json, "options")?.into_search_options();
        let matches = midx.search(pattern, &opts)?;
        let dtos: Vec<MatchDto> = matches.iter().map(MatchDto::from_search_match).collect();
        owned_json(&dtos)
    })
}
