// SyntextIndex: the native on-disk index over a project directory.

import Foundation
import CSyntext

/// Encode any FFI input DTO to a JSON string.
func encodeJSON<T: Encodable>(_ v: T) throws -> String {
    String(data: try SyntextJSON.encoder.encode(v), encoding: .utf8) ?? "{}"
}

/// Searchable index over a repository directory (build once, search many).
///
/// Wraps the Rust `Index`: an on-disk n-gram index under `indexDir` with
/// snapshot isolation, so searches are safe concurrently with commits.
///
/// Threading: `@unchecked Sendable` because the Rust handle is `Send + Sync`
/// (statically asserted in the Rust source) and the class stores nothing
/// else. All calls are blocking; call them off the main thread.
public final class SyntextIndex: @unchecked Sendable {
    private var handle: OpaquePointer?
    private var freed = false

    private init(handle: OpaquePointer?) {
        self.handle = handle
    }

    deinit {
        syntext_index_free(handle)
    }

    /// Build a fresh index by walking `repoRoot` (gitignore-aware) into
    /// `indexDir`. Overwrites any prior index there.
    public static func build(
        indexDir: String,
        repoRoot: String,
        config: SyntextConfig = SyntextConfig()
    ) throws -> SyntextIndex {
        let cIndexDir = CZString(indexDir)
        let cRepoRoot = CZString(repoRoot)
        let cConfig = try? CZString(encodeJSON(config))
        let handle = try ffiCall { err in
            syntext_index_build(cIndexDir?.ptr, cRepoRoot?.ptr, cConfig?.ptr, err)
        }
        return SyntextIndex(handle: handle)
    }

    /// Open an existing index (shared lock; readers can coexist). Throws
    /// `SyntextError.indexError(code: 2)` when no index exists at `indexDir`.
    public convenience init(
        indexDir: String,
        repoRoot: String,
        config: SyntextConfig = SyntextConfig()
    ) throws {
        let cIndexDir = CZString(indexDir)
        let cRepoRoot = CZString(repoRoot)
        let cConfig = try? CZString(encodeJSON(config))
        let handle = try ffiCall { err in
            syntext_index_open(cIndexDir?.ptr, cRepoRoot?.ptr, cConfig?.ptr, err)
        }
        self.init(handle: handle)
    }

    /// Search for a literal or regex pattern. Blocking.
    public func search(
        _ pattern: String,
        options: SyntextSearchOptions = SyntextSearchOptions()
    ) throws -> [SyntextSearchMatch] {
        try searchImpl(pattern, options: options) { cPattern, cOptions, err in
            syntext_index_search(handle, cPattern, cOptions, err)
        }
    }

    /// Bounded git auto-update, then search. Requires `git` on PATH; on a
    /// non-git directory the update degrades to `.noChanges`. Blocking.
    public func searchFresh(
        _ pattern: String,
        options: SyntextSearchOptions = SyntextSearchOptions(),
        limits: SyntextUpdateLimits? = nil
    ) throws -> SyntextSearchResult {
        let cPattern = CZString(pattern)
        let cOptions = try? CZString(encodeJSON(options))
        let cLimits = try? CZString(limits.map(encodeJSON))
        let json = try ffiCall { err in
            syntext_index_search_fresh(handle, cPattern?.ptr, cOptions?.ptr, cLimits?.ptr, err)
        }
        let data = Data(takeString(json).utf8)
        let dto = try SyntextJSON.decoder.decode(SearchFreshDto.self, from: data)
        return SyntextSearchResult(
            matches: dto.matches.map(SyntextSearchMatch.init(dto:)),
            outcome: SyntextUpdateOutcome(dto: dto.updateOutcome)
        )
    }

    /// Index statistics.
    public func stats() throws -> SyntextStats {
        let json = try ffiCall { err in syntext_index_stats(handle, err) }
        let dto = try SyntextJSON.decoder.decode(StatsDto.self, from: Data(takeString(json).utf8))
        return SyntextStats(dto: dto)
    }

    /// Bounded git change detection, applied to the index. Requires `git` on
    /// PATH. Blocking.
    public func updateFromGit(limits: SyntextUpdateLimits? = nil) throws -> SyntextUpdateOutcome {
        let cLimits = try? CZString(limits.map(encodeJSON))
        let json = try ffiCall { err in
            syntext_index_update_from_git(handle, cLimits?.ptr, err)
        }
        let dto = try SyntextJSON.decoder.decode(UpdateOutcomeDto.self, from: Data(takeString(json).utf8))
        return SyntextUpdateOutcome(dto: dto)
    }

    /// Buffer a file change. `path` must be an absolute path under the repo
    /// root (a bare repo-relative path is rejected with `PathOutsideRepo`,
    /// code 6, since resolution strips the repo root as a prefix). Not
    /// visible to `search` until `commitBatch()`.
    public func notifyChange(_ path: String) throws {
        try statusCall(path) { cPath, err in
            syntext_index_notify_change(handle, cPath, err)
        }
    }

    /// Buffer a file deletion. Not visible to `search` until `commitBatch()`.
    public func notifyDelete(_ path: String) throws {
        try statusCall(path) { cPath, err in
            syntext_index_notify_delete(handle, cPath, err)
        }
    }

    /// Apply all buffered notifications atomically (snapshot swap). Blocking.
    public func commitBatch() throws {
        let rc = try ffiCall { err in syntext_index_commit_batch(handle, err) }
        try checkStatus(rc)
    }

    /// Full checksum verification of all base segments (O(index) I/O).
    public func verify() throws {
        let rc = try ffiCall { err in syntext_index_verify(handle, err) }
        try checkStatus(rc)
    }

    // ── Internals ─────────────────────────────────────────────
    private func searchImpl(
        _ pattern: String,
        options: SyntextSearchOptions,
        _ call: (UnsafePointer<CChar>?, UnsafePointer<CChar>?, UnsafeMutablePointer<OpaquePointer?>) throws -> UnsafeMutablePointer<CChar>?
    ) throws -> [SyntextSearchMatch] {
        let cPattern = CZString(pattern)
        let cOptions = try? CZString(encodeJSON(options))
        let json = try ffiCall { err in
            try call(cPattern?.ptr, cOptions?.ptr, err)
        }
        let dtos = try SyntextJSON.decoder.decode([MatchDto].self, from: Data(takeString(json).utf8))
        return dtos.map(SyntextSearchMatch.init(dto:))
    }

    private func statusCall(
        _ path: String,
        _ call: (UnsafePointer<CChar>?, UnsafeMutablePointer<OpaquePointer?>) throws -> Int32
    ) throws {
        let cPath = CZString(path)
        let rc = try ffiCall { err in
            try call(cPath?.ptr, err)
        }
        try checkStatus(rc)
    }

    private func checkStatus(_ rc: Int32) throws {
        if rc != SYNTEXT_OK {
            throw SyntextError.indexError(code: UInt32(rc), message: "operation failed (code \(rc))")
        }
    }
}
