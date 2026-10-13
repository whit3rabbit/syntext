//! C ABI for the context engine (`ffi` feature), for Swift and other
//! foreign hosts.
//!
//! This ABI extends syntext's own (`syntext::ffi`) and is compiled into the
//! same static library, so a host links one Rust library and sees one
//! convention. The contract, mirrored by the hand-written
//! `swift/Sources/CSyntext/include/syntext_context.h`:
//!
//! - Handles are opaque pointers owned by the caller and released with
//!   their `*_free` function; all `*_free` functions are NULL-safe. Engine
//!   and memory handles keep their store alive, so handles may be freed in
//!   any order.
//! - JSON-returning functions return an owned `char*` (free it with
//!   `syntext_string_free`) or NULL on error, with `*err_out` set to a
//!   `syntext_error` (free with `syntext_error_free`). `int32_t` functions
//!   return `SYNTEXT_OK` or an error code.
//! - JSON in and out is snake_case; unknown input fields are ignored, and
//!   output fields are only ever added.
//! - Every handle is thread-safe; any function may be called from any
//!   thread. Calls block: keep them off a UI thread.
//! - Rust panics are caught at this boundary and surface as
//!   `SYNTEXT_ERR_PANIC`.
//!
//! This module is the only place in the crate allowed `unsafe`: reading C
//! strings, dereferencing handles, and calling host callbacks cannot be
//! expressed without it. Each use carries its safety contract.

// The entry points take raw pointers by design; their contracts are
// documented per function and in the header. Marking them `unsafe fn`
// would push the same uncheckable contract onto every C caller for no
// added safety, as in syntext's own FFI module.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

mod dto;
mod embed;
mod engine;
mod store;
mod tools;

use std::ffi::{c_void, CStr, CString};
use std::os::raw::c_char;
use std::sync::{Arc, OnceLock};

pub use embed::*;
pub use engine::*;
pub use store::*;
use syntext::ffi::{FfiError, SYNTEXT_ERR_INVALID_ARGUMENT, SYNTEXT_ERR_IO};
pub use tools::*;

use crate::error::ContextError;
use crate::memory::ConversationMemory;
use crate::store::ContextStore;
use crate::tokens::TokenCounter;
use crate::ContextEngine;

// ── Stable error codes (append-only; never renumber) ────────
// Context errors use their own range so syntext's codes can keep growing.
// I/O, invalid-argument, panic, and syntext index errors use syntext's.

/// Another process holds the store's writer lock (retryable).
pub const SYNTEXT_CONTEXT_ERR_LOCKED: u32 = 300;
/// A store file failed validation; recompact, resync, or rebuild.
pub const SYNTEXT_CONTEXT_ERR_CORRUPT: u32 = 301;
/// No store exists at the directory.
pub const SYNTEXT_CONTEXT_ERR_NOT_FOUND: u32 = 302;
/// A write through a read-only handle.
pub const SYNTEXT_CONTEXT_ERR_READ_ONLY: u32 = 303;
/// The store was written by an incompatible format version.
pub const SYNTEXT_CONTEXT_ERR_UNSUPPORTED_VERSION: u32 = 304;
/// The system prompt and question alone exceed the model window.
pub const SYNTEXT_CONTEXT_ERR_WINDOW_EXCEEDED: u32 = 305;
/// A sync was cancelled from its progress callback; nothing was published.
pub const SYNTEXT_CONTEXT_ERR_CANCELLED: u32 = 306;
/// The host's embedding callback failed or returned malformed vectors.
pub const SYNTEXT_CONTEXT_ERR_EMBEDDER: u32 = 307;

/// The ABI error for a context error.
pub(crate) fn ffi_error(e: ContextError) -> FfiError {
    let code = match &e {
        ContextError::Io(_) => SYNTEXT_ERR_IO,
        ContextError::Corrupt(_) => SYNTEXT_CONTEXT_ERR_CORRUPT,
        ContextError::UnsupportedVersion { .. } => SYNTEXT_CONTEXT_ERR_UNSUPPORTED_VERSION,
        ContextError::Locked(_) => SYNTEXT_CONTEXT_ERR_LOCKED,
        ContextError::ReadOnly => SYNTEXT_CONTEXT_ERR_READ_ONLY,
        ContextError::NotFound(_) => SYNTEXT_CONTEXT_ERR_NOT_FOUND,
        ContextError::InvalidInput(_) => SYNTEXT_ERR_INVALID_ARGUMENT,
        ContextError::WindowExceeded { .. } => SYNTEXT_CONTEXT_ERR_WINDOW_EXCEEDED,
        ContextError::Cancelled => SYNTEXT_CONTEXT_ERR_CANCELLED,
        ContextError::Embedder(_) => SYNTEXT_CONTEXT_ERR_EMBEDDER,
        ContextError::Index(_) => {
            let ContextError::Index(e) = e else {
                unreachable!("matched above")
            };
            return FfiError::from(e);
        }
    };
    FfiError::new(code, e.to_string())
}

// ── Opaque handles ──────────────────────────────────────────
// Zero-sized markers: C sees only pointers. The allocations behind them are
// `Box<Arc<ContextStore>>`, `Box<ContextEngine>`, and
// `Box<ConversationMemory>`.

/// Opaque handle to a context store.
#[allow(non_camel_case_types)]
#[repr(C)]
pub struct syntext_context_store {
    _private: [u8; 0],
}

/// Opaque handle to a context engine.
#[allow(non_camel_case_types)]
#[repr(C)]
pub struct syntext_context_engine {
    _private: [u8; 0],
}

/// Opaque handle to one conversation's memory.
#[allow(non_camel_case_types)]
#[repr(C)]
pub struct syntext_context_memory {
    _private: [u8; 0],
}

/// Borrow the store behind a handle.
///
/// # Safety
/// `ptr` must be NULL or a live handle from `syntext_context_store_open`.
pub(crate) unsafe fn borrow_store<'a>(
    ptr: *const syntext_context_store,
) -> Result<&'a Arc<ContextStore>, FfiError> {
    (ptr as *const Arc<ContextStore>)
        .as_ref()
        .ok_or_else(|| FfiError::invalid_arg("store handle is NULL"))
}

/// Borrow the engine behind a handle.
///
/// # Safety
/// `ptr` must be NULL or a live handle from `syntext_context_engine_new`.
pub(crate) unsafe fn borrow_engine<'a>(
    ptr: *const syntext_context_engine,
) -> Result<&'a ContextEngine, FfiError> {
    (ptr as *const ContextEngine)
        .as_ref()
        .ok_or_else(|| FfiError::invalid_arg("engine handle is NULL"))
}

/// Borrow the memory behind a handle; `Ok(None)` for NULL.
///
/// # Safety
/// `ptr` must be NULL or a live handle from `syntext_context_memory_open`.
pub(crate) unsafe fn borrow_memory<'a>(
    ptr: *const syntext_context_memory,
) -> Option<&'a ConversationMemory> {
    (ptr as *const ConversationMemory).as_ref()
}

// ── Host callbacks ──────────────────────────────────────────

/// Sync progress: called after each chunk of observed paths with the
/// counts so far. Return `true` to continue, `false` to cancel (the sync
/// then fails with `SYNTEXT_CONTEXT_ERR_CANCELLED` and publishes nothing).
/// Called on the thread running the sync.
#[allow(non_camel_case_types)]
pub type syntext_context_progress_fn = Option<
    unsafe extern "C" fn(userdata: *mut c_void, observed: u64, total: u64, changed: u64) -> bool,
>;

/// Token counter: the number of tokens the `len` UTF-8 bytes at `text`
/// encode to with the model's tokenizer (`text` is not NUL-terminated).
/// May be called from any thread, concurrently.
#[allow(non_camel_case_types)]
pub type syntext_context_count_fn =
    Option<unsafe extern "C" fn(userdata: *mut c_void, text: *const c_char, len: usize) -> u64>;

/// A host pointer passed back to host callbacks.
#[derive(Clone, Copy)]
pub(crate) struct UserData(pub *mut c_void);

// SAFETY: the pointer is only ever handed back to the host's own callback.
// The header requires `userdata` to stay valid while the callback can run
// and the callback to be safe to call from any thread, which is exactly
// what sharing it across threads needs.
unsafe impl Send for UserData {}
unsafe impl Sync for UserData {}

/// A [`TokenCounter`] backed by a host callback.
pub(crate) struct CallbackCounter {
    pub count: unsafe extern "C" fn(*mut c_void, *const c_char, usize) -> u64,
    pub userdata: UserData,
    pub name: String,
}

impl TokenCounter for CallbackCounter {
    fn count(&self, text: &str) -> usize {
        // SAFETY: `text` points to `text.len()` initialized bytes for the
        // duration of the call; the callback contract is the header's.
        let n = unsafe { (self.count)(self.userdata.0, text.as_ptr().cast(), text.len()) };
        usize::try_from(n).unwrap_or(usize::MAX)
    }

    fn name(&self) -> &str {
        &self.name
    }
}

// ── Version ─────────────────────────────────────────────────
const VERSION: &CStr =
    match CStr::from_bytes_with_nul(concat!(env!("CARGO_PKG_VERSION"), "\0").as_bytes()) {
        Ok(c) => c,
        Err(_) => panic!("version literal is not NUL-terminated"),
    };

/// Version of the context engine as a static NUL-terminated string (never
/// freed). `syntext_version` reports the syntext library it is built with.
#[no_mangle]
pub extern "C" fn syntext_context_version() -> *const c_char {
    VERSION.as_ptr()
}

/// The evidence policy engines append to the system prompt on turns with
/// evidence, as a static NUL-terminated string (never freed). A host that
/// keeps its system prompt constant (for KV-cache reuse) includes this once
/// and creates engines with `"append_policy": false`.
#[no_mangle]
pub extern "C" fn syntext_context_evidence_policy() -> *const c_char {
    static POLICY: OnceLock<CString> = OnceLock::new();
    POLICY
        .get_or_init(|| {
            CString::new(crate::render::EVIDENCE_POLICY).unwrap_or_else(|_| CString::default())
        })
        .as_ptr()
}
