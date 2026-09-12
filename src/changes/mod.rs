//! Reusable change tracking, content fingerprinting, and generation-tagged batches.
//!
//! Provides a persistent file catalogue that maps repository paths to settled
//! cryptographic content digests (BLAKE3) and observed generations. Downstream
//! consumers (e.g. synrepo, symbol graphs, embedding pipelines) can inspect
//! fine-grained change records to avoid redundant work when files are merely
//! touched or unchanged.

pub mod batch;
pub mod catalogue;
pub(crate) mod codec;
pub mod fingerprint;

pub use batch::{ChangeBatch, ChangeKind, ChangeRecord};
pub use catalogue::{Catalogue, CATALOGUE_FILENAME, DEFAULT_MAX_RETAIN_BYTES};
pub use fingerprint::{ContentDigest, FileFingerprint, StatObservation, RACY_MARGIN};
