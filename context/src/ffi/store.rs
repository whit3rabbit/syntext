//! `extern "C"` entry points for stores and conversation memory.

use std::ffi::c_void;
use std::os::raw::c_char;
use std::path::PathBuf;
use std::sync::Arc;

use syntext::ffi::{
    catch, cstr, ffi_ptr, ffi_status, owned_json, req_str, syntext_error, FfiError,
};

use super::dto::{DocDto, SourceDto, StoreOptionsDto};
use super::{
    borrow_memory, borrow_store, ffi_error, syntext_context_memory, syntext_context_progress_fn,
    syntext_context_store, UserData,
};
use crate::memory::{ConversationMemory, MemoryMode};
use crate::store::ContextStore;
use crate::types::{DocKey, Role};

/// Parse a required JSON argument.
///
/// # Safety
/// `ptr` must be NULL or a NUL-terminated string valid for the call.
pub(crate) unsafe fn req_json<T: serde::de::DeserializeOwned>(
    ptr: *const c_char,
    what: &str,
) -> Result<T, FfiError> {
    let s = req_str(ptr, what)?;
    serde_json::from_str(s).map_err(|e| FfiError::invalid_arg(format!("invalid {what} JSON: {e}")))
}

/// Parse a nullable JSON argument (NULL means `T::default()`).
///
/// # Safety
/// `ptr` must be NULL or a NUL-terminated string valid for the call.
pub(crate) unsafe fn opt_json<T: Default + serde::de::DeserializeOwned>(
    ptr: *const c_char,
    what: &str,
) -> Result<T, FfiError> {
    match cstr(ptr)? {
        None => Ok(T::default()),
        Some(s) => serde_json::from_str(s)
            .map_err(|e| FfiError::invalid_arg(format!("invalid {what} JSON: {e}"))),
    }
}

/// Open the store in `dir`, creating it when absent unless
/// `options_json.must_exist`. `options_json` may be NULL. Release with
/// `syntext_context_store_free`.
#[no_mangle]
pub extern "C" fn syntext_context_store_open(
    dir: *const c_char,
    options_json: *const c_char,
    err_out: *mut *mut syntext_error,
) -> *mut syntext_context_store {
    ffi_ptr(err_out, || unsafe {
        let dir = PathBuf::from(req_str(dir, "dir")?);
        let opts: StoreOptionsDto = opt_json(options_json, "options")?;
        let options = opts.options();
        let store = if opts.read_only {
            ContextStore::open_read_only(&dir, options)
        } else if opts.must_exist {
            ContextStore::open(&dir, options)
        } else {
            ContextStore::open_or_create(&dir, options)
        }
        .map_err(ffi_error)?;
        Ok(Box::into_raw(Box::new(Arc::new(store))) as *mut syntext_context_store)
    })
}

/// Release a store handle. Engines and memories opened on it keep the store
/// alive. NULL-safe.
#[no_mangle]
pub extern "C" fn syntext_context_store_free(store: *mut syntext_context_store) {
    let _ = catch(|| {
        if !store.is_null() {
            drop(unsafe { Box::from_raw(store as *mut Arc<ContextStore>) });
        }
        Ok(())
    });
}

/// Bring the documents of a directory source in line with the directory.
/// `source_json`: `{"label", "root", "scope"?, "trust"?, "max_file_bytes"?,
/// "include_nested_checkouts"?, "exclude"?}`. `progress` may be NULL;
/// `userdata` is passed to it. Returns a `SyncReport` JSON.
#[no_mangle]
pub extern "C" fn syntext_context_store_sync(
    store: *const syntext_context_store,
    source_json: *const c_char,
    progress: syntext_context_progress_fn,
    userdata: *mut c_void,
    err_out: *mut *mut syntext_error,
) -> *mut c_char {
    ffi_ptr(err_out, || unsafe {
        let store = borrow_store(store)?;
        let source = req_json::<SourceDto>(source_json, "source")?
            .source()
            .map_err(ffi_error)?;
        let ud = UserData(userdata);
        let report = store
            .sync_directory_with(&source, |p| match progress {
                // SAFETY: the host passed this callback and its userdata for
                // the duration of this call.
                Some(f) => f(ud.0, p.observed as u64, p.total as u64, p.changed as u64),
                None => true,
            })
            .map_err(ffi_error)?;
        owned_json(&report)
    })
}

/// Bring only the paths in `paths_json` (a JSON array of absolute paths
/// under the source root, or relative ones) in line with the directory,
/// without walking it. Returns a `SyncReport` JSON.
#[no_mangle]
pub extern "C" fn syntext_context_store_sync_paths(
    store: *const syntext_context_store,
    source_json: *const c_char,
    paths_json: *const c_char,
    err_out: *mut *mut syntext_error,
) -> *mut c_char {
    ffi_ptr(err_out, || unsafe {
        let store = borrow_store(store)?;
        let source = req_json::<SourceDto>(source_json, "source")?
            .source()
            .map_err(ffi_error)?;
        let paths: Vec<PathBuf> = req_json(paths_json, "paths")?;
        let report = store.sync_paths(&source, &paths).map_err(ffi_error)?;
        owned_json(&report)
    })
}

/// Compile and publish host documents, replacing ones with the same keys.
/// `docs_json`: an array of `{"id", "text", "external"?, "title"?,
/// "trust"?, "scope"?, "timestamp"?}`. Returns an `IngestReport` JSON.
#[no_mangle]
pub extern "C" fn syntext_context_store_upsert(
    store: *const syntext_context_store,
    docs_json: *const c_char,
    err_out: *mut *mut syntext_error,
) -> *mut c_char {
    ffi_ptr(err_out, || unsafe {
        let store = borrow_store(store)?;
        let docs: Vec<DocDto> = req_json(docs_json, "docs")?;
        let docs = docs
            .into_iter()
            .map(DocDto::doc)
            .collect::<crate::Result<Vec<_>>>()
            .map_err(ffi_error)?;
        owned_json(&store.upsert(docs).map_err(ffi_error)?)
    })
}

/// Remove documents by key (`keys_json`: an array of keys such as
/// `"note:x"`). Unknown keys are ignored. Returns an `IngestReport` JSON.
#[no_mangle]
pub extern "C" fn syntext_context_store_remove(
    store: *const syntext_context_store,
    keys_json: *const c_char,
    err_out: *mut *mut syntext_error,
) -> *mut c_char {
    ffi_ptr(err_out, || unsafe {
        let store = borrow_store(store)?;
        let keys: Vec<String> = req_json(keys_json, "keys")?;
        let keys = keys
            .into_iter()
            .map(DocKey::new)
            .collect::<crate::Result<Vec<_>>>()
            .map_err(ffi_error)?;
        owned_json(&store.remove(&keys).map_err(ffi_error)?)
    })
}

/// Compact when due (`force == false`) or always. Returns a `CompactReport`
/// JSON, or `null` when nothing was due.
#[no_mangle]
pub extern "C" fn syntext_context_store_compact(
    store: *const syntext_context_store,
    force: bool,
    err_out: *mut *mut syntext_error,
) -> *mut c_char {
    ffi_ptr(err_out, || unsafe {
        let store = borrow_store(store)?;
        let report = if force {
            Some(store.compact())
        } else {
            store.maybe_compact().transpose()
        };
        owned_json(&report.transpose().map_err(ffi_error)?)
    })
}

/// Current statistics as a `StoreStats` JSON.
#[no_mangle]
pub extern "C" fn syntext_context_store_stats(
    store: *const syntext_context_store,
    err_out: *mut *mut syntext_error,
) -> *mut c_char {
    ffi_ptr(err_out, || unsafe {
        owned_json(&borrow_store(store)?.stats())
    })
}

/// Read-only handles: pick up generations another process published.
/// `*changed_out` (nullable) is set to whether anything changed.
#[no_mangle]
pub extern "C" fn syntext_context_store_reload(
    store: *const syntext_context_store,
    changed_out: *mut bool,
    err_out: *mut *mut syntext_error,
) -> i32 {
    ffi_status(err_out, || unsafe {
        let changed = borrow_store(store)?.reload().map_err(ffi_error)?;
        if !changed_out.is_null() {
            *changed_out = changed;
        }
        Ok(())
    })
}

/// Open a conversation, reloading its persisted turns. `mode_json`:
/// `{"project": "<id>"}`, `{"app": "<id>"}`, or `"off"` (nothing
/// persists). Release with `syntext_context_memory_free`.
#[no_mangle]
pub extern "C" fn syntext_context_memory_open(
    store: *const syntext_context_store,
    session: *const c_char,
    mode_json: *const c_char,
    err_out: *mut *mut syntext_error,
) -> *mut syntext_context_memory {
    ffi_ptr(err_out, || unsafe {
        let store = borrow_store(store)?;
        let session = req_str(session, "session")?;
        let mode: MemoryMode = req_json(mode_json, "mode")?;
        let memory =
            ConversationMemory::resume(Arc::clone(store), session, mode).map_err(ffi_error)?;
        Ok(Box::into_raw(Box::new(memory)) as *mut syntext_context_memory)
    })
}

/// Release a memory handle. NULL-safe.
#[no_mangle]
pub extern "C" fn syntext_context_memory_free(memory: *mut syntext_context_memory) {
    let _ = catch(|| {
        if !memory.is_null() {
            drop(unsafe { Box::from_raw(memory as *mut ConversationMemory) });
        }
        Ok(())
    });
}

/// Durably record one turn. `role`: `"user"`, `"assistant"`, `"system"`,
/// or `"tool"`.
#[no_mangle]
pub extern "C" fn syntext_context_memory_record(
    memory: *const syntext_context_memory,
    role: *const c_char,
    text: *const c_char,
    err_out: *mut *mut syntext_error,
) -> i32 {
    ffi_status(err_out, || unsafe {
        let memory =
            borrow_memory(memory).ok_or_else(|| FfiError::invalid_arg("memory handle is NULL"))?;
        let role: Role = serde_json::from_value(serde_json::Value::String(
            req_str(role, "role")?.to_string(),
        ))
        .map_err(|_| FfiError::invalid_arg("role must be user, assistant, system, or tool"))?;
        let text = req_str(text, "text")?;
        memory.record(role, text).map_err(ffi_error)
    })
}

/// Every turn of the conversation, oldest first, as a JSON array of
/// `{"role", "content"}`.
#[no_mangle]
pub extern "C" fn syntext_context_memory_history(
    memory: *const syntext_context_memory,
    err_out: *mut *mut syntext_error,
) -> *mut c_char {
    ffi_ptr(err_out, || unsafe {
        let memory =
            borrow_memory(memory).ok_or_else(|| FfiError::invalid_arg("memory handle is NULL"))?;
        owned_json(&memory.history())
    })
}
