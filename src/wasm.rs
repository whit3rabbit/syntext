//! WASM bindings for the syntext in-memory index.
//!
//! Exposes a `WasmIndex` class to JavaScript/TypeScript:
//!
//! ```js
//! const idx = new WasmIndex({ "src/main.rs": uint8ArrayContent });
//! const matches = idx.search("fn main");
//! // matches: Array<{path, line_number, line_content, submatch_start, submatch_end}>
//! ```

use std::collections::HashMap;

use wasm_bindgen::prelude::*;

use crate::index::wasm_index::InMemoryIndex;
use crate::SearchOptions;

/// WASM-serializable match (line_content as UTF-8 string, path as string).
#[derive(serde::Serialize)]
struct WasmMatch {
    path: String,
    line_number: u32,
    line_content: String,
    submatch_start: usize,
    submatch_end: usize,
}

/// In-memory code-search index for WASM targets.
///
/// Build once from a map of `{path: Uint8Array}`, then call `search()` repeatedly.
#[wasm_bindgen]
pub struct WasmIndex {
    inner: InMemoryIndex,
}

#[wasm_bindgen]
impl WasmIndex {
    /// Construct and index the provided files.
    ///
    /// `files` must be a plain JS object whose keys are repo-relative paths
    /// and whose values are `Uint8Array` file contents.
    ///
    /// ```js
    /// const idx = new WasmIndex({ "src/lib.rs": new TextEncoder().encode("fn foo() {}") });
    /// ```
    #[wasm_bindgen(constructor)]
    pub fn new(files: JsValue) -> Result<WasmIndex, JsValue> {
        let entries = js_sys::Object::entries(files.unchecked_ref());

        let mut map: HashMap<String, Vec<u8>> = HashMap::new();
        for entry in entries.iter() {
            // Each entry is [key, value]
            let pair = js_sys::Array::from(&entry);
            let key = pair
                .get(0)
                .as_string()
                .ok_or_else(|| JsValue::from_str("file path must be a string"))?;
            let val = pair.get(1);
            let bytes = js_sys::Uint8Array::new(&val).to_vec();
            map.insert(key, bytes);
        }

        let inner = InMemoryIndex::build(map)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        Ok(WasmIndex { inner })
    }

    /// Search for `pattern` across all indexed files.
    ///
    /// Returns a JS array of objects:
    /// `[{path: string, line_number: number, line_content: string,
    ///    submatch_start: number, submatch_end: number}, ...]`
    pub fn search(&self, pattern: &str) -> Result<JsValue, JsValue> {
        let opts = SearchOptions::default();
        let matches = self
            .inner
            .search(pattern, &opts)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;

        let wasm_matches: Vec<WasmMatch> = matches
            .iter()
            .map(|m| WasmMatch {
                path: m.path.to_string_lossy().into_owned(),
                line_number: m.line_number,
                line_content: String::from_utf8_lossy(&m.line_content).into_owned(),
                submatch_start: m.submatch_start,
                submatch_end: m.submatch_end,
            })
            .collect();

        serde_wasm_bindgen::to_value(&wasm_matches)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }
}
