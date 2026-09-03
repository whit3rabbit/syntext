//! C ABI for foreign-language bindings (Swift via xcframework).
//!
//! ABI contract (mirrored 1:1 by `swift/Sources/CSyntext/include/syntext.h`):
//!
//! - Handles are opaque pointers owned by the caller. Every `*_free` releases
//!   them; all are NULL-safe.
//! - Pointer-returning functions return NULL on error and set `*err_out`
//!   (when non-NULL) to a `syntext_error` the caller releases with
//!   `syntext_error_free`. `syntext_error_message` borrows until freed.
//! - `int32_t`-returning functions return `SYNTEXT_OK` (0) or an error code.
//! - Returned `char*` values are owned JSON strings; free with
//!   `syntext_string_free`.
//! - All handles are thread-safe (the Rust types are `Send + Sync`); any
//!   function may be called from any thread.
//! - Rust panics are caught at this boundary (`catch_unwind`) and surface as
//!   `SYNTEXT_ERR_PANIC`; a panic never crosses the FFI boundary.
//!
//! Error codes are stable and append-only; never renumber them (see the
//! consts below and the matching enum in `syntext.h`).

// The extern "C" entry points (this module and its `index`/`mem` submodules)
// take raw pointers by design; their safety contracts are documented
// per-function and in syntext.h. Marking them `unsafe fn` would push the
// identical, uncheckable contract onto every C caller for no additional
// safety, so the ptr-deref lint is scoped off for the whole FFI tree.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

mod dto;
mod index;
mod mem;

use std::ffi::{CStr, CString};
use std::os::raw::c_char;

use crate::IndexError;

pub use index::*;
pub use mem::*;

// ── Opaque handle types ─────────────────────────────────────
// ZST marker types: the C side only ever sees pointers. Actual handles are
// `Box<Index>` / `Box<MemIndex>` / `Box<FfiError>` cast through raw pointers;
// the cast is address-preserving (same allocation, different pointee type).

/// Opaque handle to a native directory [`Index`](crate::index::Index).
#[allow(non_camel_case_types)]
#[repr(C)]
pub struct syntext_index {
    _private: [u8; 0],
}

/// Opaque handle to a [`MemIndex`](crate::index::mem_index::MemIndex).
#[allow(non_camel_case_types)]
#[repr(C)]
pub struct syntext_mem_index {
    _private: [u8; 0],
}

/// Opaque handle to an FFI error payload.
#[allow(non_camel_case_types)]
#[repr(C)]
pub struct syntext_error {
    _private: [u8; 0],
}

// ── Stable error codes (append-only; never renumber) ───────
/// Success.
pub const SYNTEXT_OK: u32 = 0;
/// `IndexError::Io`
pub const SYNTEXT_ERR_IO: u32 = 1;
/// `IndexError::IndexNotFound`
pub const SYNTEXT_ERR_INDEX_NOT_FOUND: u32 = 2;
/// `IndexError::InvalidPattern`
pub const SYNTEXT_ERR_INVALID_PATTERN: u32 = 3;
/// `IndexError::CorruptIndex`
pub const SYNTEXT_ERR_CORRUPT_INDEX: u32 = 4;
/// `IndexError::QueryTooBroad`
pub const SYNTEXT_ERR_QUERY_TOO_BROAD: u32 = 5;
/// `IndexError::PathOutsideRepo`
pub const SYNTEXT_ERR_PATH_OUTSIDE_REPO: u32 = 6;
/// `IndexError::FileTooLarge`
pub const SYNTEXT_ERR_FILE_TOO_LARGE: u32 = 7;
/// `IndexError::LockConflict` (retryable: re-acquire with backoff)
pub const SYNTEXT_ERR_LOCK_CONFLICT: u32 = 8;
/// `IndexError::OverlayFull`
pub const SYNTEXT_ERR_OVERLAY_FULL: u32 = 9;
/// `IndexError::DocIdOverflow`
pub const SYNTEXT_ERR_DOC_ID_OVERFLOW: u32 = 10;
/// Boundary misuse: NULL handle, non-UTF-8 or NULL required input, bad JSON.
pub const SYNTEXT_ERR_INVALID_ARGUMENT: u32 = 100;
/// A Rust panic was caught at the boundary.
pub const SYNTEXT_ERR_PANIC: u32 = 101;
/// An `IndexError` variant this ABI version does not know.
pub const SYNTEXT_ERR_UNKNOWN: u32 = 200;

/// Default and hard cap applied to `max_results` at the FFI boundary.
/// Bounds the owned JSON string and match materialization for callers we
/// cannot audit (an FFI caller is no more trusted than a shell user).
pub(crate) const DEFAULT_MAX_RESULTS: usize = 10_000;
pub(crate) const MAX_MAX_RESULTS: usize = 1_000_000;

/// Default bounded-update limits when `limits_json` is NULL: the same values
/// the `st` CLI uses for search-time auto-update.
pub(crate) const DEFAULT_LIMITS: crate::index::freshness::UpdateLimits =
    crate::index::freshness::UpdateLimits {
        max_files: Some(200),
        budget_ms: Some(150),
    };

// ── Error payload ───────────────────────────────────────────
/// Error payload crossing the FFI boundary.
pub(crate) struct FfiError {
    code: u32,
    /// Stored NUL-terminated so `syntext_error_message` can lend a `char*`.
    message: CString,
}

impl FfiError {
    pub(crate) fn invalid_arg(msg: impl Into<String>) -> Self {
        Self::new(SYNTEXT_ERR_INVALID_ARGUMENT, msg)
    }

    fn new(code: u32, msg: impl Into<String>) -> Self {
        FfiError {
            code,
            // Display impls never emit interior NUL in practice; fall back to
            // a static message rather than panicking at the boundary.
            message: CString::new(msg.into())
                .unwrap_or_else(|_| CString::new("error message contained NUL").unwrap()),
        }
    }

    fn panic(payload: Box<dyn std::any::Any + Send>) -> Self {
        let msg = payload
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown payload".to_string());
        Self::new(SYNTEXT_ERR_PANIC, format!("rust panic: {msg}"))
    }
}

impl From<IndexError> for FfiError {
    fn from(e: IndexError) -> Self {
        // IndexError is #[non_exhaustive]; this in-crate match is exhaustive
        // today, so any newly added variant is a compile error here — the
        // cue to assign it a stable ABI code and extend syntext.h.
        let code = match &e {
            IndexError::Io(_) => SYNTEXT_ERR_IO,
            IndexError::IndexNotFound(_) => SYNTEXT_ERR_INDEX_NOT_FOUND,
            IndexError::InvalidPattern(_) => SYNTEXT_ERR_INVALID_PATTERN,
            IndexError::CorruptIndex(_) => SYNTEXT_ERR_CORRUPT_INDEX,
            IndexError::QueryTooBroad { .. } => SYNTEXT_ERR_QUERY_TOO_BROAD,
            IndexError::PathOutsideRepo(_) => SYNTEXT_ERR_PATH_OUTSIDE_REPO,
            IndexError::FileTooLarge { .. } => SYNTEXT_ERR_FILE_TOO_LARGE,
            IndexError::LockConflict(_) => SYNTEXT_ERR_LOCK_CONFLICT,
            IndexError::OverlayFull { .. } => SYNTEXT_ERR_OVERLAY_FULL,
            IndexError::DocIdOverflow { .. } => SYNTEXT_ERR_DOC_ID_OVERFLOW,
        };
        FfiError::new(code, e.to_string())
    }
}

// ── Panic firewall + call wrappers ──────────────────────────
/// Run `f`, converting an unwinding panic into an `Err` at the FFI boundary.
///
/// Uses `AssertUnwindSafe`: proving `UnwindSafe` for every closure borrowing
/// `&Index` is not tractable, and no memory unsafety results — the handle is
/// not mutated across the unwind and the panicked call's state is discarded.
/// This is the standard C-API pattern; no `unsafe` is involved. Panic
/// messages still print to stderr via the default hook; we deliberately do
/// NOT install a global `set_hook` (a process-global side effect the host
/// application owns).
pub(crate) fn catch<T>(f: impl FnOnce() -> Result<T, FfiError>) -> Result<T, FfiError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
        .unwrap_or_else(|payload| Err(FfiError::panic(payload)))
}

/// Wrap a pointer-returning FFI body: NULL on error, `*err_out` set.
pub(crate) fn ffi_ptr<T>(
    err_out: *mut *mut syntext_error,
    f: impl FnOnce() -> Result<*mut T, FfiError>,
) -> *mut T {
    match catch(f) {
        Ok(v) => v,
        Err(e) => {
            set_err(err_out, e);
            std::ptr::null_mut()
        }
    }
}

/// Wrap an `int32_t`-returning FFI body: `SYNTEXT_OK` or the error code.
pub(crate) fn ffi_status(
    err_out: *mut *mut syntext_error,
    f: impl FnOnce() -> Result<(), FfiError>,
) -> i32 {
    match catch(f) {
        Ok(()) => SYNTEXT_OK as i32,
        Err(e) => {
            let code = e.code;
            set_err(err_out, e);
            code as i32
        }
    }
}

/// Store `e` into `*err_out` as a fresh owned error handle.
pub(crate) fn set_err(err_out: *mut *mut syntext_error, e: FfiError) {
    if !err_out.is_null() {
        // err_out is an out-parameter the caller allocated for this call.
        unsafe { *err_out = Box::into_raw(Box::new(e)) as *mut syntext_error };
    }
}

// ── Input helpers (the `unsafe` boundary) ───────────────────
/// Read a nullable C string argument as UTF-8. `None` means NULL.
///
/// # Safety
/// `ptr` must be NULL or point to a valid NUL-terminated C string that stays
/// valid for the duration of this call.
pub(crate) unsafe fn cstr<'a>(ptr: *const c_char) -> Result<Option<&'a str>, FfiError> {
    if ptr.is_null() {
        return Ok(None);
    }
    let bytes = CStr::from_ptr(ptr).to_bytes();
    std::str::from_utf8(bytes)
        .map(Some)
        .map_err(|_| FfiError::invalid_arg("input string is not valid UTF-8"))
}

/// Read a required (non-NULL) C string argument as UTF-8.
///
/// # Safety
/// Same contract as [`cstr`], minus NULL.
pub(crate) unsafe fn req_str<'a>(ptr: *const c_char, what: &str) -> Result<&'a str, FfiError> {
    cstr(ptr)?.ok_or_else(|| FfiError::invalid_arg(format!("{what} must not be NULL")))
}

/// Read a `(ptr, len)` byte-slice argument. Content bytes may contain NUL and
/// invalid UTF-8 by design (chat content).
///
/// # Safety
/// `ptr` must be NULL (allowed only when `len == 0`) or point to at least
/// `len` readable bytes valid for the duration of this call.
pub(crate) unsafe fn bytes<'a>(ptr: *const u8, len: usize) -> Result<&'a [u8], FfiError> {
    if ptr.is_null() {
        if len == 0 {
            return Ok(&[]);
        }
        return Err(FfiError::invalid_arg(
            "content pointer is NULL but content_len > 0",
        ));
    }
    if len > isize::MAX as usize {
        return Err(FfiError::invalid_arg("content_len exceeds isize::MAX"));
    }
    Ok(std::slice::from_raw_parts(ptr, len))
}

/// Serialize `v` to an owned NUL-terminated JSON string.
pub(crate) fn owned_json<T: serde::Serialize>(v: &T) -> Result<*mut c_char, FfiError> {
    let s = serde_json::to_string(v).map_err(|e| {
        FfiError::new(
            SYNTEXT_ERR_UNKNOWN,
            format!("serializing FFI response failed: {e}"),
        )
    })?;
    // serde_json escapes control characters, so an interior NUL is impossible;
    // mapped defensively instead of unwrapping at the boundary.
    CString::new(s)
        .map(CString::into_raw)
        .map_err(|_| FfiError::invalid_arg("JSON response contained interior NUL"))
}

/// Parse a nullable JSON argument; NULL means `T::default()`.
pub(crate) fn parse_opt_json<T: Default + serde::de::DeserializeOwned>(
    ptr: *const c_char,
    what: &str,
) -> Result<T, FfiError> {
    match unsafe { cstr(ptr)? } {
        None => Ok(T::default()),
        Some(s) => serde_json::from_str(s)
            .map_err(|e| FfiError::invalid_arg(format!("invalid {what} JSON: {e}"))),
    }
}

// ── Handle borrows ──────────────────────────────────────────
/// Borrow the [`Index`](crate::index::Index) behind a handle.
///
/// # Safety
/// `ptr` must be a handle returned by `syntext_index_build`/`open` that has
/// not yet been passed to `syntext_index_free`.
pub(crate) unsafe fn borrow_index<'a>(
    ptr: *const syntext_index,
) -> Result<&'a crate::index::Index, FfiError> {
    (ptr as *const crate::index::Index)
        .as_ref()
        .ok_or_else(|| FfiError::invalid_arg("index handle is NULL"))
}

/// Borrow the [`MemIndex`](crate::index::mem_index::MemIndex) behind a handle.
///
/// # Safety
/// `ptr` must be a handle returned by `syntext_mem_index_new` that has not
/// yet been passed to `syntext_mem_index_free`.
pub(crate) unsafe fn borrow_mem<'a>(
    ptr: *const syntext_mem_index,
) -> Result<&'a crate::index::mem_index::MemIndex, FfiError> {
    (ptr as *const crate::index::mem_index::MemIndex)
        .as_ref()
        .ok_or_else(|| FfiError::invalid_arg("mem index handle is NULL"))
}

/// Borrow the [`FfiError`] behind a handle.
///
/// # Safety
/// `ptr` must be NULL or a handle set by a previous FFI call that has not yet
/// been passed to `syntext_error_free`.
pub(crate) unsafe fn borrow_error<'a>(ptr: *const syntext_error) -> Option<&'a FfiError> {
    (ptr as *const FfiError).as_ref()
}

// ── Shared entry points ─────────────────────────────────────
const VERSION: &CStr =
    match CStr::from_bytes_with_nul(concat!(env!("CARGO_PKG_VERSION"), "\0").as_bytes()) {
        Ok(c) => c,
        // concat! always appends exactly one NUL; unreachable by construction.
        Err(_) => panic!("version literal is not NUL-terminated"),
    };

/// Return the crate version as a static NUL-terminated string (never freed).
#[no_mangle]
pub extern "C" fn syntext_version() -> *const c_char {
    VERSION.as_ptr()
}

/// Free an owned JSON string returned by any syntext FFI function. NULL-safe.
#[no_mangle]
pub extern "C" fn syntext_string_free(s: *mut c_char) {
    if !s.is_null() {
        drop(unsafe { CString::from_raw(s) });
    }
}

/// Numeric error code of `err` (`SYNTEXT_ERR_UNKNOWN` for NULL).
#[no_mangle]
pub extern "C" fn syntext_error_code(err: *const syntext_error) -> u32 {
    unsafe { borrow_error(err) }
        .map(|e| e.code)
        .unwrap_or(SYNTEXT_ERR_UNKNOWN)
}

/// Borrowed NUL-terminated message of `err` (NULL for NULL). Valid until the
/// error is passed to `syntext_error_free`.
#[no_mangle]
pub extern "C" fn syntext_error_message(err: *const syntext_error) -> *const c_char {
    unsafe { borrow_error(err) }
        .map(|e| e.message.as_ptr())
        .unwrap_or(std::ptr::null())
}

/// Free an error handle set by `*err_out`. NULL-safe.
#[no_mangle]
pub extern "C" fn syntext_error_free(err: *mut syntext_error) {
    // See syntext_index_free: route the drop through the panic firewall.
    let _ = catch(|| {
        if !err.is_null() {
            drop(unsafe { Box::from_raw(err as *mut FfiError) });
        }
        Ok(())
    });
}
