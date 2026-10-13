// SyntextContextStore: compiled documents, conversation memory, and the
// change catalogue of every synced directory, in one directory on disk.

import Foundation
import CSyntext
import Syntext

/// A context store: the offline half of the engine. Sources are compiled
/// once into immutable segments; questions are answered from them without
/// re-reading the corpus.
///
/// One writer per store directory at a time (another process gets an error
/// for which `isContextStoreLocked` is true); any number of read-only
/// handles. Threading: `@unchecked Sendable` because the Rust store is
/// `Send + Sync` and the class holds only its handle. Calls block; use the
/// `async` variants or call off the main thread.
public final class SyntextContextStore: @unchecked Sendable {
    let handle: OpaquePointer?

    /// Open the store in `directory`, creating it when absent (unless
    /// `options.mustExist`).
    public init(directory: String, options: SyntextContextStoreOptions = .init()) throws {
        let cDir = CZString(directory)
        let cOptions = try CZString(encodeJSON(options))
        handle = try ffiCall { err in
            syntext_context_store_open(cDir?.ptr, cOptions?.ptr, err)
        }
    }

    deinit {
        syntext_context_store_free(handle)
    }

    /// Bring the documents of `source` in line with its directory. Only
    /// files whose content changed are read and compiled; the whole sync is
    /// published at once. `progress` is called after each chunk of paths;
    /// returning `false` cancels (the call then throws an error for which
    /// `isCancelled` is true, and nothing is published).
    @discardableResult
    public func sync(
        _ source: SyntextDirectorySource,
        progress: ((SyntextSyncProgress) -> Bool)? = nil
    ) throws -> SyntextSyncReport {
        let cSource = try CZString(encodeJSON(source))
        let box = progress.map(ProgressBox.init)
        let json = try withExtendedLifetime(box) {
            try ffiCall { err in
                syntext_context_store_sync(
                    handle, cSource?.ptr, box == nil ? nil : progressTrampoline,
                    box.map { Unmanaged.passUnretained($0).toOpaque() }, err)
            }
        }
        return try decode(SyntextSyncReport.self, takeString(json))
    }

    /// `sync` off the calling thread.
    @discardableResult
    public func syncAsync(
        _ source: SyntextDirectorySource,
        progress: (@Sendable (SyntextSyncProgress) -> Bool)? = nil
    ) async throws -> SyntextSyncReport {
        try await Task.detached(priority: .utility) {
            try self.sync(source, progress: progress)
        }.value
    }

    /// Update only `paths` (absolute paths under the source root, or
    /// relative ones) without walking the directory, for files the app
    /// itself just wrote. `.gitignore` is not consulted; the next full
    /// `sync` reconciles everything else.
    @discardableResult
    public func sync(paths: [String], in source: SyntextDirectorySource) throws -> SyntextSyncReport {
        let cSource = try CZString(encodeJSON(source))
        let cPaths = try CZString(encodeJSON(paths))
        let json = try ffiCall { err in
            syntext_context_store_sync_paths(handle, cSource?.ptr, cPaths?.ptr, err)
        }
        return try decode(SyntextSyncReport.self, takeString(json))
    }

    /// Compile and publish host documents, replacing ones with the same id.
    @discardableResult
    public func upsert(_ documents: [SyntextContextDocument]) throws -> SyntextIngestReport {
        let cDocs = try CZString(encodeJSON(documents))
        let json = try ffiCall { err in syntext_context_store_upsert(handle, cDocs?.ptr, err) }
        return try decode(SyntextIngestReport.self, takeString(json))
    }

    /// Remove documents by key (`note:<id>`, `ext:<uri>`, `<label>:<path>`).
    @discardableResult
    public func remove(keys: [String]) throws -> SyntextIngestReport {
        let cKeys = try CZString(encodeJSON(keys))
        let json = try ffiCall { err in syntext_context_store_remove(handle, cKeys?.ptr, err) }
        return try decode(SyntextIngestReport.self, takeString(json))
    }

    /// Compact when due, or always with `force`. `nil` when nothing was due.
    @discardableResult
    public func compact(force: Bool = false) throws -> SyntextCompactReport? {
        let json = try ffiCall { err in syntext_context_store_compact(handle, force, err) }
        return try decode(SyntextCompactReport?.self, takeString(json))
    }

    /// Current statistics.
    public func stats() throws -> SyntextStoreStats {
        let json = try ffiCall { err in syntext_context_store_stats(handle, err) }
        return try decode(SyntextStoreStats.self, takeString(json))
    }

    /// Read-only handles: pick up what a writer in another process
    /// published. Returns whether anything changed.
    @discardableResult
    public func reload() throws -> Bool {
        var changed = false
        _ = try ffiCall { err in syntext_context_store_reload(handle, &changed, err) }
        return changed
    }
}
