// SyntextChatIndex: mutable in-memory index for chat-style documents.

import Foundation
import CSyntext

/// Searchable in-memory document index for content that does not live on
/// disk (chat transcripts, log slices, notes).
///
/// Buffer documents with `add`/`remove`, then `commit()` to publish them
/// atomically: searches started before a commit finish against the previous
/// snapshot. `commit()` is O(total indexed content) — the right trade for
/// thousands of small documents, not for large corpora (use `SyntextIndex`).
///
/// Document ids double as index paths: they must be non-empty and
/// relative-path-shaped (no leading `/`, no `..`); e.g. `chats/42/msg-7`.
/// Binary-looking content (a NUL in the first 8 KiB) is silently skipped at
/// commit, matching the file-ingestion behavior.
///
/// Threading: `@unchecked Sendable` because the Rust handle is `Send + Sync`
/// and the class stores nothing else. All calls are blocking.
public final class SyntextChatIndex: @unchecked Sendable {
    private var handle: OpaquePointer?

    /// Create an empty chat index.
    public init() throws {
        handle = try ffiCall { err in syntext_mem_index_new(err) }
    }

    deinit {
        syntext_mem_index_free(handle)
    }

    /// Buffer a document; replaces any existing entry with the same id.
    /// Not visible to `search` until `commit()`.
    public func add(_ id: String, content: Data) throws {
        let cId = CZString(id)
        let rc = try content.withUnsafeBytes { (raw: UnsafeRawBufferPointer) in
            try ffiCall { err in
                syntext_mem_index_add(
                    handle,
                    cId?.ptr,
                    raw.baseAddress?.assumingMemoryBound(to: UInt8.self),
                    raw.count,
                    err
                )
            }
        }
        try checkStatus(rc)
    }

    /// UTF-8 convenience overload of `add(_:content:)`.
    public func add(_ id: String, content: String) throws {
        try add(id, content: Data(content.utf8))
    }

    /// Buffer a document deletion (absent id is a no-op). Not visible to
    /// `search` until `commit()`.
    public func remove(_ id: String) throws {
        let cId = CZString(id)
        let rc = try ffiCall { err in
            syntext_mem_index_remove(handle, cId?.ptr, err)
        }
        try checkStatus(rc)
    }

    /// Rebuild the snapshot from all buffered documents and publish it
    /// atomically. O(total content); blocks `add`/`remove` while running.
    public func commit() throws {
        let rc = try ffiCall { err in syntext_mem_index_commit(handle, err) }
        try checkStatus(rc)
    }

    /// Search the committed snapshot for a literal or regex pattern. Blocking.
    public func search(
        _ pattern: String,
        options: SyntextSearchOptions = SyntextSearchOptions()
    ) throws -> [SyntextSearchMatch] {
        let cPattern = CZString(pattern)
        let cOptions = try? CZString(encodeJSON(options))
        let json = try ffiCall { err in
            syntext_mem_index_search(handle, cPattern?.ptr, cOptions?.ptr, err)
        }
        let dtos = try SyntextJSON.decoder.decode([MatchDto].self, from: Data(takeString(json).utf8))
        return dtos.map(SyntextSearchMatch.init(dto:))
    }

    private func checkStatus(_ rc: Int32) throws {
        if rc != SYNTEXT_OK {
            throw SyntextError.indexError(code: UInt32(rc), message: "operation failed (code \(rc))")
        }
    }
}
