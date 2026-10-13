//! `extern "C"` entry points for semantic retrieval: a host embedding
//! callback wrapped as an embedder handle, and the store's embedding job.

use std::ffi::c_void;
use std::os::raw::c_char;
use std::sync::Arc;

use serde::Deserialize;
use syntext::ffi::{catch, ffi_ptr, owned_json, req_str, syntext_error, FfiError};

use super::store::opt_json;
use super::{
    borrow_store, ffi_error, syntext_context_progress_fn, syntext_context_store, UserData,
};
use crate::embed::{EmbedOptions, Embedder, MAX_DIMS, MAX_ID_BYTES};

/// Opaque handle to a host embedding model.
#[allow(non_camel_case_types)]
#[repr(C)]
pub struct syntext_context_embedder {
    _private: [u8; 0],
}

/// Embedding callback: write `count * dims` floats to `out`, one vector per
/// text, for the `count` texts (`texts[i]` is `lens[i]` UTF-8 bytes, not
/// NUL-terminated). `is_query` is true for a question, false for passages.
/// Return `true` on success; `false` fails the call that needed vectors.
/// May be called from any thread, concurrently.
#[allow(non_camel_case_types)]
pub type syntext_context_embed_fn = Option<
    unsafe extern "C" fn(
        userdata: *mut c_void,
        texts: *const *const c_char,
        lens: *const usize,
        count: usize,
        is_query: bool,
        out: *mut f32,
    ) -> bool,
>;

/// An [`Embedder`] backed by a host callback.
struct CallbackEmbedder {
    id: String,
    dims: usize,
    embed: unsafe extern "C" fn(
        *mut c_void,
        *const *const c_char,
        *const usize,
        usize,
        bool,
        *mut f32,
    ) -> bool,
    userdata: UserData,
}

impl CallbackEmbedder {
    fn call(&self, texts: &[&str], is_query: bool) -> std::result::Result<Vec<Vec<f32>>, String> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let ptrs: Vec<*const c_char> = texts.iter().map(|t| t.as_ptr().cast()).collect();
        let lens: Vec<usize> = texts.iter().map(|t| t.len()).collect();
        let mut out = vec![f32::NAN; texts.len() * self.dims];
        // SAFETY: `ptrs[i]` points to `lens[i]` initialized bytes and `out`
        // to `count * dims` writable floats, all alive for the call; the
        // callback's own contract is the header's.
        let ok = unsafe {
            (self.embed)(
                self.userdata.0,
                ptrs.as_ptr(),
                lens.as_ptr(),
                texts.len(),
                is_query,
                out.as_mut_ptr(),
            )
        };
        if !ok {
            return Err("host embedding callback failed".to_string());
        }
        if out.iter().any(|x| !x.is_finite()) {
            return Err("host embedding callback left non-finite values".to_string());
        }
        Ok(out.chunks(self.dims).map(<[f32]>::to_vec).collect())
    }
}

impl Embedder for CallbackEmbedder {
    fn id(&self) -> &str {
        &self.id
    }

    fn dims(&self) -> usize {
        self.dims
    }

    fn embed_documents(&self, texts: &[&str]) -> std::result::Result<Vec<Vec<f32>>, String> {
        self.call(texts, false)
    }

    fn embed_query(&self, text: &str) -> std::result::Result<Vec<f32>, String> {
        let mut v = self.call(&[text], true)?;
        v.pop().ok_or_else(|| "no vector".to_string())
    }
}

/// Borrow the embedder behind a handle; `Ok(None)` for NULL.
///
/// # Safety
/// `ptr` must be NULL or a live handle from `syntext_context_embedder_new`.
pub(crate) unsafe fn borrow_embedder<'a>(
    ptr: *const syntext_context_embedder,
) -> Option<&'a Arc<dyn Embedder>> {
    (ptr as *const Arc<dyn Embedder>).as_ref()
}

/// Wrap a host embedding model. `id` names the model and every setting that
/// changes its vectors (vectors are only compared within one id); `dims`
/// is the vector length. `userdata` is passed to `embed` and must stay
/// valid until this handle and every engine created with it are freed.
/// Release with `syntext_context_embedder_free`.
#[no_mangle]
pub extern "C" fn syntext_context_embedder_new(
    id: *const c_char,
    dims: usize,
    embed: syntext_context_embed_fn,
    userdata: *mut c_void,
    err_out: *mut *mut syntext_error,
) -> *mut syntext_context_embedder {
    ffi_ptr(err_out, || unsafe {
        let id = req_str(id, "id")?;
        if id.is_empty() || id.len() > MAX_ID_BYTES || id.chars().any(char::is_control) {
            return Err(FfiError::invalid_arg(
                "id must be 1-256 bytes, no control characters",
            ));
        }
        if dims == 0 || dims > MAX_DIMS {
            return Err(FfiError::invalid_arg("dims must be 1-4096"));
        }
        let embed = embed.ok_or_else(|| FfiError::invalid_arg("embed callback is NULL"))?;
        let e: Arc<dyn Embedder> = Arc::new(CallbackEmbedder {
            id: id.to_string(),
            dims,
            embed,
            userdata: UserData(userdata),
        });
        Ok(Box::into_raw(Box::new(e)) as *mut syntext_context_embedder)
    })
}

/// Release an embedder handle. Engines created with it keep their own
/// reference. NULL-safe.
#[no_mangle]
pub extern "C" fn syntext_context_embedder_free(embedder: *mut syntext_context_embedder) {
    let _ = catch(|| {
        if !embedder.is_null() {
            drop(unsafe { Box::from_raw(embedder as *mut Arc<dyn Embedder>) });
        }
        Ok(())
    });
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct EmbedOptionsDto {
    batch: Option<usize>,
    max_passages: Option<usize>,
    prune_other_models: Option<bool>,
}

/// Give live passages vectors from `embedder`, prose first, reusing the
/// vector of any text embedded before. `options_json` (nullable):
/// `{"batch"?, "max_passages"?, "prune_other_models"?}`. `progress`
/// (nullable) is called after each batch as `(userdata, done, total, 0)`;
/// returning false stops the run early, keeping what was embedded. Returns
/// `{"model", "embedded", "reused", "covered", "remaining", "cancelled"}`.
#[no_mangle]
pub extern "C" fn syntext_context_store_embed(
    store: *const syntext_context_store,
    embedder: *const syntext_context_embedder,
    options_json: *const c_char,
    progress: syntext_context_progress_fn,
    userdata: *mut c_void,
    err_out: *mut *mut syntext_error,
) -> *mut c_char {
    ffi_ptr(err_out, || unsafe {
        let store = borrow_store(store)?;
        let embedder = borrow_embedder(embedder)
            .ok_or_else(|| FfiError::invalid_arg("embedder handle is NULL"))?;
        let dto: EmbedOptionsDto = opt_json(options_json, "options")?;
        let d = EmbedOptions::default();
        let opts = EmbedOptions {
            batch: dto.batch.unwrap_or(d.batch).max(1),
            max_passages: dto.max_passages,
            prune_other_models: dto.prune_other_models.unwrap_or(d.prune_other_models),
        };
        let ud = UserData(userdata);
        let report = store
            .embed_with(embedder.as_ref(), &opts, |p| match progress {
                // SAFETY: the callback contract is the header's.
                Some(f) => f(ud.0, p.done as u64, p.total as u64, 0),
                None => true,
            })
            .map_err(ffi_error)?;
        owned_json(&report)
    })
}
