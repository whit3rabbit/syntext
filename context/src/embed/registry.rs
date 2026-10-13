//! Loaded vector sidecars, shared by every query on a store handle.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use super::embedder_key;
use super::table::{sidecar_name, VectorTable};
use crate::store::StoreSnapshot;

/// Vector tables by segment file name.
pub(crate) type Tables = Arc<HashMap<String, Arc<VectorTable>>>;

struct Loaded {
    /// Generation whose durable segments were looked up.
    generation: u64,
    tables: Tables,
}

/// Per embedder: the sidecars of the current segments. Sidecars are a
/// cache, so one that is missing or fails validation just leaves its
/// segment without vectors.
#[derive(Default)]
pub(crate) struct VectorCache {
    inner: Mutex<HashMap<String, Loaded>>,
}

impl std::fmt::Debug for VectorCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VectorCache").finish_non_exhaustive()
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl VectorCache {
    /// The tables of `snap`'s durable segments for embedder `id`, reading
    /// sidecars of segments not seen before.
    pub(crate) fn tables(&self, dir: &Path, snap: &StoreSnapshot, id: &str) -> Tables {
        let key = embedder_key(id);
        let mut inner = lock(&self.inner);
        if let Some(l) = inner.get(&key) {
            if l.generation == snap.generation {
                return Arc::clone(&l.tables);
            }
        }
        let previous = inner.get(&key).map(|l| Arc::clone(&l.tables));
        let mut map = HashMap::new();
        for seg in snap.segments.iter().filter(|s| s.durable) {
            if let Some(t) = previous.as_ref().and_then(|p| p.get(&seg.name)) {
                map.insert(seg.name.clone(), Arc::clone(t));
                continue;
            }
            let path = dir.join(sidecar_name(&seg.name, &key));
            match std::fs::read(&path) {
                Ok(bytes) => match VectorTable::decode(&bytes, id, seg.reader.passages.len()) {
                    Ok(t) => {
                        map.insert(seg.name.clone(), Arc::new(t));
                    }
                    Err(e) => log::warn!("ignoring {}: {e}", path.display()),
                },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => log::warn!("ignoring {}: {e}", path.display()),
            }
        }
        let tables = Arc::new(map);
        inner.insert(
            key,
            Loaded {
                generation: snap.generation,
                tables: Arc::clone(&tables),
            },
        );
        tables
    }

    /// Make tables just written visible to later queries.
    pub(crate) fn insert(&self, id: &str, written: Vec<(String, Arc<VectorTable>)>) {
        let key = embedder_key(id);
        let mut inner = lock(&self.inner);
        if let Some(l) = inner.get_mut(&key) {
            let mut map = (*l.tables).clone();
            map.extend(written);
            l.tables = Arc::new(map);
        }
    }

    /// Forget every loaded table (another process may have written new
    /// sidecars).
    pub(crate) fn clear(&self) {
        lock(&self.inner).clear();
    }
}
