// SyntextContextEngine: retrieval, evidence packing, and prompt assembly
// for a model with a small context window.

import Foundation
import CSyntext
import Syntext

/// Chooses evidence from a store for each question and assembles prompts
/// that fit the model's window.
///
/// Evidence is always placed in a user message as `<evidence>` blocks, never
/// in the system role; the system prompt only gains a short policy telling
/// the model evidence is reference data. Threading: `@unchecked Sendable`
/// (the Rust engine is `Send + Sync`); calls block, so use the `async`
/// variants or call off the main thread.
public final class SyntextContextEngine: @unchecked Sendable {
    private let handle: OpaquePointer?
    private let counter: Unmanaged<CounterBox>?
    /// Kept alive while the engine can call it.
    private let embedder: SyntextContextEmbedder?

    /// Create an engine over `store`. Pass `tokenCounter` to count tokens
    /// with the model's own tokenizer (strongly recommended: without it a
    /// conservative estimate is used). It may be called from any thread.
    /// Pass `embedder` for semantic retrieval over the vectors
    /// `store.embed(_:)` stored with the same embedder.
    public init(
        store: SyntextContextStore,
        options: SyntextContextEngineOptions = .init(),
        tokenCounter: (@Sendable (String) -> Int)? = nil,
        embedder: SyntextContextEmbedder? = nil
    ) throws {
        let cOptions = try CZString(encodeJSON(options))
        let box = tokenCounter.map { Unmanaged.passRetained(CounterBox($0)) }
        do {
            handle = try withExtendedLifetime(embedder) {
                try ffiCall { err in
                    syntext_context_engine_new(
                        store.handle, cOptions?.ptr, box == nil ? nil : countTrampoline,
                        box?.toOpaque(), embedder?.handle, err)
                }
            }
        } catch {
            box?.release()
            throw error
        }
        counter = box
        self.embedder = embedder
    }

    deinit {
        // The engine may call the counter until it is freed.
        syntext_context_engine_free(handle)
        counter?.release()
    }

    /// The policy engines append to the system prompt on turns with
    /// evidence: evidence is reference data, never instructions, and must
    /// be cited. Include it in a constant system prompt and set
    /// `appendPolicy: false` to keep the prompt prefix cacheable.
    public static var evidencePolicy: String {
        String(cString: syntext_context_evidence_policy())
    }

    /// Evidence for `question` within an explicit token budget, for hosts
    /// that manage their own history and prompt (insert `evidenceText` into
    /// the last user message). With `conversation`, older turns are
    /// recalled as evidence; its `keptTurns` most recent turns, which the
    /// host already has verbatim in its prompt, are left out.
    public func prepare(
        question: String,
        evidenceTokens: Int,
        conversation: SyntextConversation? = nil,
        keptTurns: Int = 0,
        scopes: [String]? = nil,
        exclude: [String] = []
    ) throws -> SyntextEvidencePacket {
        let request = PrepareRequestDto(
            question: question, evidenceTokens: evidenceTokens, keptTurns: keptTurns,
            scopes: scopes, exclude: exclude)
        let cRequest = try CZString(encodeJSON(request))
        let json = try withExtendedLifetime(conversation) {
            try ffiCall { err in
                syntext_context_prepare(handle, conversation?.handle, cRequest?.ptr, err)
            }
        }
        return try decode(PacketDto.self, takeString(json)).packet
    }

    /// A complete prompt for a model window: system prompt plus evidence
    /// policy, recent history verbatim, then the evidence and the question.
    /// With `conversation`, history and memory scope come from it. Throws an
    /// error for which `isWindowExceeded` is true rather than returning a
    /// prompt larger than the window.
    public func assemble(
        _ request: SyntextChatRequest,
        conversation: SyntextConversation? = nil
    ) throws -> SyntextChatTurn {
        let cRequest = try CZString(encodeJSON(request))
        let json = try withExtendedLifetime(conversation) {
            try ffiCall { err in
                syntext_context_assemble(handle, conversation?.handle, cRequest?.ptr, err)
            }
        }
        let dto = try decode(TurnDto.self, takeString(json))
        return SyntextChatTurn(
            messages: dto.messages, packet: dto.packet.packet, budget: dto.budget,
            historyKept: dto.historyKept, historyDropped: dto.historyDropped,
            promptTokens: dto.promptTokens)
    }

    /// Ranked passages for `question`, for a search tool the model can
    /// call. Choose `scopes` in the app, never from model output.
    public func search(
        _ question: String,
        maxResults: Int? = nil,
        scopes: [String]? = nil,
        exclude: [String] = []
    ) throws -> [SyntextPassage] {
        let request = SearchRequestDto(
            question: question, maxResults: maxResults, scopes: scopes, exclude: exclude)
        let cRequest = try CZString(encodeJSON(request))
        let json = try ffiCall { err in syntext_context_search(handle, cRequest?.ptr, err) }
        return try decode([SyntextPassage].self, takeString(json))
    }

    /// `prepare` off the calling thread.
    public func prepareAsync(
        question: String,
        evidenceTokens: Int,
        conversation: SyntextConversation? = nil,
        keptTurns: Int = 0,
        scopes: [String]? = nil,
        exclude: [String] = []
    ) async throws -> SyntextEvidencePacket {
        try await Task.detached(priority: .userInitiated) {
            try self.prepare(
                question: question, evidenceTokens: evidenceTokens, conversation: conversation,
                keptTurns: keptTurns, scopes: scopes, exclude: exclude)
        }.value
    }

    /// `assemble` off the calling thread.
    public func assembleAsync(
        _ request: SyntextChatRequest,
        conversation: SyntextConversation? = nil
    ) async throws -> SyntextChatTurn {
        try await Task.detached(priority: .userInitiated) {
            try self.assemble(request, conversation: conversation)
        }.value
    }

    /// `search` off the calling thread.
    public func searchAsync(
        _ question: String,
        maxResults: Int? = nil,
        scopes: [String]? = nil,
        exclude: [String] = []
    ) async throws -> [SyntextPassage] {
        try await Task.detached(priority: .userInitiated) {
            try self.search(question, maxResults: maxResults, scopes: scopes, exclude: exclude)
        }.value
    }

    /// Re-hash the files a receipt cites (keys `<label>:<path>` under
    /// `root`) and report which changed since the evidence was taken.
    public static func checkSources(
        receiptJSON: String,
        root: String,
        label: String
    ) throws -> [SyntextSourceCheck] {
        let cReceipt = CZString(receiptJSON)
        let cRoot = CZString(root)
        let cLabel = CZString(label)
        let json = try ffiCall { err in
            syntext_context_receipt_check(cReceipt?.ptr, cRoot?.ptr, cLabel?.ptr, err)
        }
        return try decode([SyntextSourceCheck].self, takeString(json))
    }
}
