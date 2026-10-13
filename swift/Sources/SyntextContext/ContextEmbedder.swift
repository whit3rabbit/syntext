// SyntextContextEmbedder: the host's embedding model, for semantic
// retrieval, and the store's embedding job.

import Foundation
import CSyntext
import Syntext

/// The host's embedding model.
///
/// `embed(texts, isQuery)` returns one vector of `dimensions` floats per
/// text, in order; `isQuery` is true for a question (for models that prefix
/// queries). It may be called from any thread, concurrently. `id` names the
/// model and every setting that changes its vectors: vectors are only ever
/// compared within one id, and a new id means re-embedding.
public final class SyntextContextEmbedder: @unchecked Sendable {
    let handle: OpaquePointer?
    private let box: Unmanaged<EmbedBox>
    /// The model identifier given at creation.
    public let id: String
    /// Length of every vector.
    public let dimensions: Int

    public init(
        id: String,
        dimensions: Int,
        embed: @escaping @Sendable (_ texts: [String], _ isQuery: Bool) throws -> [[Float]]
    ) throws {
        let box = Unmanaged.passRetained(EmbedBox(dimensions: dimensions, embed: embed))
        let cId = CZString(id)
        do {
            handle = try ffiCall { err in
                syntext_context_embedder_new(cId?.ptr, dimensions, embedTrampoline, box.toOpaque(), err)
            }
        } catch {
            box.release()
            throw error
        }
        self.box = box
        self.id = id
        self.dimensions = dimensions
    }

    deinit {
        // Engines keep the Swift embedder alive while they exist, so no
        // Rust reference outlives the box released here.
        syntext_context_embedder_free(handle)
        box.release()
    }
}

/// Progress of an embedding run, reported after each batch.
public struct SyntextEmbedProgress: Sendable, Equatable {
    /// Passages embedded so far in this run.
    public let done: Int
    /// Passages this run will embed.
    public let total: Int
}

/// What an embedding run did.
public struct SyntextEmbedReport: Decodable, Sendable, Equatable {
    /// The embedder's id.
    public let model: String
    /// Passages the model was called for.
    public let embedded: Int
    /// Passages that got the vector of identical text embedded earlier.
    public let reused: Int
    /// Live passages with a vector after the run.
    public let covered: Int
    /// Live passages still without one; the next run continues with them.
    public let remaining: Int
    /// The progress closure stopped the run (work done before is kept).
    public let cancelled: Bool
}

extension SyntextContextStore {
    /// Give live passages vectors from `embedder`, prose first, reusing the
    /// vector of any text embedded before: syncs and compaction never cause
    /// unchanged text to be embedded again. Resumable: `maxPassages`, a
    /// `false` from `progress`, or an error stops early and keeps what was
    /// embedded. Queries use whatever has been embedded so far.
    @discardableResult
    public func embed(
        _ embedder: SyntextContextEmbedder,
        maxPassages: Int? = nil,
        batch: Int? = nil,
        pruneOtherModels: Bool = true,
        progress: ((SyntextEmbedProgress) -> Bool)? = nil
    ) throws -> SyntextEmbedReport {
        let options = EmbedOptionsDto(
            batch: batch, maxPassages: maxPassages, pruneOtherModels: pruneOtherModels)
        let cOptions = try CZString(encodeJSON(options))
        let box = progress.map(EmbedProgressBox.init)
        let json = try withExtendedLifetime((box, embedder)) {
            try ffiCall { err in
                syntext_context_store_embed(
                    handle, embedder.handle, cOptions?.ptr,
                    box == nil ? nil : embedProgressTrampoline,
                    box.map { Unmanaged.passUnretained($0).toOpaque() }, err)
            }
        }
        return try decode(SyntextEmbedReport.self, takeString(json))
    }

    /// `embed` off the calling thread, at background priority.
    @discardableResult
    public func embedAsync(
        _ embedder: SyntextContextEmbedder,
        maxPassages: Int? = nil,
        batch: Int? = nil,
        pruneOtherModels: Bool = true,
        progress: (@Sendable (SyntextEmbedProgress) -> Bool)? = nil
    ) async throws -> SyntextEmbedReport {
        try await Task.detached(priority: .background) {
            try self.embed(
                embedder, maxPassages: maxPassages, batch: batch,
                pruneOtherModels: pruneOtherModels, progress: progress)
        }.value
    }
}

extension SyntextError {
    /// The embedding closure threw or returned vectors of the wrong shape.
    public var isEmbedderFailure: Bool {
        if case .indexError(let code, _) = self { return code == UInt32(SYNTEXT_CONTEXT_ERR_EMBEDDER) }
        return false
    }
}

// ── Plumbing ────────────────────────────────────────────────

struct EmbedOptionsDto: Encodable {
    let batch: Int?
    let maxPassages: Int?
    let pruneOtherModels: Bool
}

/// Holds an embedding closure for the C callback.
final class EmbedBox {
    let dimensions: Int
    let embed: @Sendable ([String], Bool) throws -> [[Float]]
    init(dimensions: Int, embed: @escaping @Sendable ([String], Bool) throws -> [[Float]]) {
        self.dimensions = dimensions
        self.embed = embed
    }
}

/// C trampoline into an `EmbedBox` passed as userdata.
let embedTrampoline: syntext_context_embed_fn = { userdata, texts, lens, count, isQuery, out in
    guard let userdata, let texts, let lens, let out else { return false }
    let box = Unmanaged<EmbedBox>.fromOpaque(userdata).takeUnretainedValue()
    var strings: [String] = []
    strings.reserveCapacity(count)
    for i in 0..<count {
        guard let p = texts[i] else { return false }
        strings.append(String(decoding: UnsafeRawBufferPointer(start: p, count: lens[i]), as: UTF8.self))
    }
    guard let vectors = try? box.embed(strings, isQuery),
          vectors.count == count,
          vectors.allSatisfy({ $0.count == box.dimensions })
    else { return false }
    for (i, v) in vectors.enumerated() {
        for (j, x) in v.enumerated() {
            out[i * box.dimensions + j] = x
        }
    }
    return true
}

/// Holds an embedding progress closure for the C callback (call-scoped).
final class EmbedProgressBox {
    let report: (SyntextEmbedProgress) -> Bool
    init(_ report: @escaping (SyntextEmbedProgress) -> Bool) { self.report = report }
}

/// C trampoline into an `EmbedProgressBox`: `(done, total, 0)`.
let embedProgressTrampoline: syntext_context_progress_fn = { userdata, done, total, _ in
    guard let userdata else { return true }
    let box = Unmanaged<EmbedProgressBox>.fromOpaque(userdata).takeUnretainedValue()
    return box.report(SyntextEmbedProgress(done: Int(clamping: done), total: Int(clamping: total)))
}
