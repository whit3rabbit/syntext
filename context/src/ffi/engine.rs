//! `extern "C"` entry points for engines (retrieval, packing, prompt
//! assembly) and receipts.

use std::ffi::c_void;
use std::os::raw::c_char;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use syntext::ffi::{catch, ffi_ptr, owned_json, req_str, syntext_error};

use super::dto::{
    AssembleDto, EngineOptionsDto, PacketDto, PassageDto, PrepareDto, SearchDto, TurnDto,
};
use super::store::{opt_json, req_json};
use super::{
    borrow_engine, borrow_memory, borrow_store, ffi_error, syntext_context_count_fn,
    syntext_context_engine, syntext_context_memory, syntext_context_store, CallbackCounter,
    UserData,
};
use crate::engine::{ChatRequest, ContextEngine, PrepareRequest};
use crate::pack::PackOptions;
use crate::receipt::Receipt;
use crate::render::RenderOptions;
use crate::retrieve::{RetrieveOptions, SyntextSource};
use crate::types::Scope;

/// Create an engine over `store`. `options_json` (nullable): `{"model"?,
/// "tokenizer"?, "syntext"?: {"index_dir", "repo_root", "label", "scope"?,
/// "trust"?}, "hops"?, "max_results"?, "max_blocks"?, "per_doc_cap"?,
/// "relevance_floor"?, "expand"?, "order"?, "placement"?, "append_policy"?}`.
///
/// `count` (nullable) counts tokens with the model's tokenizer; without it
/// a conservative estimate is used. `count_userdata` is passed to `count`
/// and must stay valid until the engine is freed. Release with
/// `syntext_context_engine_free`.
#[no_mangle]
pub extern "C" fn syntext_context_engine_new(
    store: *const syntext_context_store,
    options_json: *const c_char,
    count: syntext_context_count_fn,
    count_userdata: *mut c_void,
    err_out: *mut *mut syntext_error,
) -> *mut syntext_context_engine {
    ffi_ptr(err_out, || unsafe {
        let store = borrow_store(store)?;
        let opts: EngineOptionsDto = opt_json(options_json, "options")?;
        let mut builder = ContextEngine::builder(Arc::clone(store));
        if let Some(f) = count {
            builder = builder.token_counter(Arc::new(CallbackCounter {
                count: f,
                userdata: UserData(count_userdata),
                name: opts.tokenizer.clone().unwrap_or_else(|| "host".to_string()),
            }));
        }
        if let Some(model) = &opts.model {
            builder = builder.model(model.clone());
        }
        if let Some(s) = &opts.syntext {
            let config =
                syntext::Config::new(PathBuf::from(&s.index_dir), PathBuf::from(&s.repo_root));
            let index = syntext::index::Index::open(config)?;
            let mut source = SyntextSource::new(Arc::new(index), &s.label).map_err(ffi_error)?;
            if let Some(scope) = &s.scope {
                source = source.with_scope(Scope::new(scope.as_str()).map_err(ffi_error)?);
            }
            if let Some(t) = s.trust {
                source = source.with_trust(t);
            }
            builder = builder.syntext(source);
        }
        let mut retrieve = RetrieveOptions::default();
        if let Some(h) = opts.hops {
            retrieve.hops = h;
        }
        if let Some(n) = opts.max_results {
            retrieve.max_results = n.max(1);
        }
        let d = PackOptions::default();
        let pack = PackOptions {
            max_blocks: opts.max_blocks.unwrap_or(d.max_blocks),
            per_doc_cap: opts.per_doc_cap.unwrap_or(d.per_doc_cap),
            relevance_floor: opts.relevance_floor.unwrap_or(d.relevance_floor),
            expand: opts.expand.unwrap_or(d.expand),
            order: opts.order.unwrap_or(d.order),
            ..d
        };
        let mut render = RenderOptions::default();
        if let Some(p) = opts.placement {
            render.placement = p;
        }
        if opts.append_policy == Some(false) {
            render.system_policy = None;
        }
        let engine = builder
            .retrieve_options(retrieve)
            .pack_options(pack)
            .render_options(render)
            .build();
        Ok(Box::into_raw(Box::new(engine)) as *mut syntext_context_engine)
    })
}

/// Release an engine handle. NULL-safe.
#[no_mangle]
pub extern "C" fn syntext_context_engine_free(engine: *mut syntext_context_engine) {
    let _ = catch(|| {
        if !engine.is_null() {
            drop(unsafe { Box::from_raw(engine as *mut ContextEngine) });
        }
        Ok(())
    });
}

/// Evidence for a question within an explicit token budget, for hosts that
/// manage their own history. `request_json`: `{"question",
/// "evidence_tokens", "scopes"?, "exclude"?}`. Returns an evidence packet
/// JSON: `{"blocks", "evidence_text", "evidence_tokens", "budget",
/// "lowest_trust", "receipt", "receipt_json"}`.
#[no_mangle]
pub extern "C" fn syntext_context_prepare(
    engine: *const syntext_context_engine,
    request_json: *const c_char,
    err_out: *mut *mut syntext_error,
) -> *mut c_char {
    ffi_ptr(err_out, || unsafe {
        let engine = borrow_engine(engine)?;
        let req: PrepareDto = req_json(request_json, "request")?;
        let packet = engine
            .prepare(&PrepareRequest {
                question: &req.question,
                evidence_tokens: req.evidence_tokens,
                scopes: req.filter.scopes().map_err(ffi_error)?,
                exclude: req.filter.exclude().map_err(ffi_error)?,
            })
            .map_err(ffi_error)?;
        owned_json(&PacketDto::new(&packet))
    })
}

/// A complete prompt for a model window. `memory` (nullable) supplies the
/// history and its scope; otherwise `history` from the request is used.
/// `request_json`: `{"question", "window", "system"?, "history"?,
/// "generation_reserve"?, "history_share"?, "scopes"?, "exclude"?}`.
/// Returns `{"messages", "packet", "budget", "history_kept",
/// "history_dropped", "prompt_tokens"}`; fails with
/// `SYNTEXT_CONTEXT_ERR_WINDOW_EXCEEDED` instead of returning a prompt
/// larger than the window.
#[no_mangle]
pub extern "C" fn syntext_context_assemble(
    engine: *const syntext_context_engine,
    memory: *const syntext_context_memory,
    request_json: *const c_char,
    err_out: *mut *mut syntext_error,
) -> *mut c_char {
    ffi_ptr(err_out, || unsafe {
        let engine = borrow_engine(engine)?;
        let memory = borrow_memory(memory);
        let req: AssembleDto = req_json(request_json, "request")?;
        let mut chat = ChatRequest::new(&req.question, req.window).history(&req.history);
        chat.system = req.system.as_deref();
        chat.memory = memory;
        chat.generation_reserve = req.generation_reserve;
        if let Some(share) = req.history_share {
            chat.history_share = share;
        }
        chat.scopes = req.filter.scopes().map_err(ffi_error)?;
        chat.exclude = req.filter.exclude().map_err(ffi_error)?;
        let turn = engine.assemble(&chat).map_err(ffi_error)?;
        owned_json(&TurnDto {
            messages: &turn.messages,
            packet: PacketDto::new(&turn.packet),
            budget: &turn.budget,
            history_kept: turn.history_kept,
            history_dropped: turn.history_dropped,
            prompt_tokens: turn.prompt_tokens,
        })
    })
}

/// Ranked passages for a question, for a search tool the model can call.
/// `request_json`: `{"question", "max_results"?, "scopes"?, "exclude"?}`.
/// Returns an array of `{"doc_key", "source", "breadcrumb", "symbol",
/// "kind", "trust", "scope", "lines", "score", "text", "live"}`.
#[no_mangle]
pub extern "C" fn syntext_context_search(
    engine: *const syntext_context_engine,
    request_json: *const c_char,
    err_out: *mut *mut syntext_error,
) -> *mut c_char {
    ffi_ptr(err_out, || unsafe {
        let engine = borrow_engine(engine)?;
        let req: SearchDto = req_json(request_json, "request")?;
        let mut opts = engine.retrieve_options().clone();
        if let Some(n) = req.max_results {
            opts.max_results = n.max(1);
        }
        if let Some(scopes) = req.filter.scopes().map_err(ffi_error)? {
            opts.scopes = scopes;
        }
        opts.exclude
            .extend(req.filter.exclude().map_err(ffi_error)?);
        let found = engine.retrieve(&req.question, &opts).map_err(ffi_error)?;
        let passages: Vec<PassageDto<'_>> = found.passages.iter().map(PassageDto::new).collect();
        owned_json(&passages)
    })
}

/// Re-hash the files a receipt cites (documents keyed `<label>:<path>`
/// under `root`) and report which changed since. Returns an array of
/// `{"id", "doc_key", "status"}` with status `unchanged`, `changed`,
/// `missing`, or `not_checked`.
#[no_mangle]
pub extern "C" fn syntext_context_receipt_check(
    receipt_json: *const c_char,
    root: *const c_char,
    label: *const c_char,
    err_out: *mut *mut syntext_error,
) -> *mut c_char {
    ffi_ptr(err_out, || unsafe {
        let receipt = Receipt::from_json(req_str(receipt_json, "receipt")?).map_err(ffi_error)?;
        let root = req_str(root, "root")?;
        let label = req_str(label, "label")?;
        owned_json(&receipt.check_sources(Path::new(root), label))
    })
}
