// Public models of prompts and evidence: chat messages, evidence packets,
// receipts, engine options, and the wire DTOs behind them.

import Foundation
import CSyntext
import Syntext

/// Speaker of a chat message.
public enum SyntextChatRole: String, Codable, Sendable {
    case system, user, assistant, tool
}

/// One chat message, shaped like an OpenAI-compatible `{role, content}`.
public struct SyntextChatMessage: Codable, Sendable, Equatable {
    public var role: SyntextChatRole
    public var content: String

    public init(role: SyntextChatRole, content: String) {
        self.role = role
        self.content = content
    }

    public static func user(_ content: String) -> Self { .init(role: .user, content: content) }
    public static func assistant(_ content: String) -> Self { .init(role: .assistant, content: content) }
    public static func system(_ content: String) -> Self { .init(role: .system, content: content) }
}

/// One block of evidence as it appears in the prompt.
public struct SyntextEvidenceBlock: Decodable, Sendable, Equatable {
    /// Citation id (`E1`, `E2`, ...).
    public let id: String
    public let docKey: String
    /// Path, or title for non-file sources.
    public let source: String
    public let breadcrumb: String
    public let symbol: String?
    /// First and last line covered (1-based).
    public let lines: [Int]
    /// Passage kind: `prose`, `code`, `definition`, `data`, or `turn`.
    public let kind: String
    public let trust: SyntextTrust
    public let scope: String
    /// Sanitized text as rendered.
    public let text: String
    public let score: Double
    /// Human-readable graph chain that reached this block.
    public let via: String?
    /// Parts were elided with explicit markers.
    public let condensed: Bool
    /// Widened beyond the retrieved passages.
    public let expanded: Bool
    /// Read from the live file rather than the store.
    public let live: Bool
    /// Rendered size in tokens.
    public let tokens: Int
}

/// Provenance of an evidence packet. The full receipt is `receiptJSON` on
/// the packet; this is the part most hosts display.
public struct SyntextReceipt: Decodable, Sendable, Equatable {
    public let version: Int
    public let createdUnix: Int64
    public let storeId: String
    public let generation: UInt64
    public let question: String
    /// `locate`, `explain`, `fact`, `recall`, or `general`.
    public let intent: String
    public let terms: [String]
    public let entities: [String]
    public let evidenceBudget: Int
    public let evidenceTokens: Int
    public let candidatesConsidered: Int
    public let tokenizer: String
    public let model: String?
}

/// Evidence chosen for one question.
public struct SyntextEvidencePacket: Sendable {
    /// Blocks in prompt order.
    public let blocks: [SyntextEvidenceBlock]
    /// Rendered evidence section (empty when there is none).
    public let evidenceText: String
    public let evidenceTokens: Int
    public let budget: Int
    /// Least-trusted source in the packet; gate tools on it.
    public let lowestTrust: SyntextTrust?
    public let receipt: SyntextReceipt
    /// The complete receipt, ready to store with the answer.
    public let receiptJSON: String

    public var isEmpty: Bool { blocks.isEmpty }
}

/// How the window was spent.
public struct SyntextBudget: Decodable, Sendable, Equatable {
    public let window: Int
    public let system: Int
    public let history: Int
    public let question: Int
    public let generation: Int
    public let margin: Int
}

/// A prompt request for a model window.
public struct SyntextChatRequest: Encodable, Sendable {
    public var question: String
    /// Model context window in tokens.
    public var window: Int
    /// Host system prompt.
    public var system: String?
    /// Prior messages (ignored when a conversation is passed).
    public var history: [SyntextChatMessage]
    /// Tokens reserved for the answer (default: a quarter of the window).
    public var generationReserve: Int?
    /// Largest share of the window spent on verbatim history (default 0.3).
    public var historyShare: Double?
    /// Scopes to read (default `["default"]`; a conversation adds its own).
    public var scopes: [String]?
    /// Document keys to leave out.
    public var exclude: [String]

    public init(
        question: String,
        window: Int,
        system: String? = nil,
        history: [SyntextChatMessage] = [],
        generationReserve: Int? = nil,
        historyShare: Double? = nil,
        scopes: [String]? = nil,
        exclude: [String] = []
    ) {
        self.question = question
        self.window = window
        self.system = system
        self.history = history
        self.generationReserve = generationReserve
        self.historyShare = historyShare
        self.scopes = scopes
        self.exclude = exclude
    }
}

/// An assembled prompt.
public struct SyntextChatTurn: Sendable {
    /// Messages to send to the model.
    public let messages: [SyntextChatMessage]
    public let packet: SyntextEvidencePacket
    public let budget: SyntextBudget
    public let historyKept: Int
    /// Older messages left out (still retrievable through memory).
    public let historyDropped: Int
    public let promptTokens: Int
}

/// One ranked passage, for search tools.
public struct SyntextPassage: Decodable, Sendable, Equatable {
    public let docKey: String
    public let source: String
    public let breadcrumb: String
    public let symbol: String?
    public let kind: String
    public let trust: SyntextTrust
    public let scope: String
    public let lines: [Int]
    public let score: Double
    public let text: String
    public let live: Bool
}

/// Whether a cited source changed since the evidence was taken.
public enum SyntextSourceStatus: String, Decodable, Sendable {
    case unchanged, changed, missing
    case notChecked = "not_checked"
}

/// Result of checking one evidence block's source.
public struct SyntextSourceCheck: Decodable, Sendable, Equatable {
    public let id: String
    public let docKey: String
    public let status: SyntextSourceStatus
}

/// Order of evidence blocks in the prompt.
public enum SyntextEvidenceOrder: String, Codable, Sendable {
    /// Best first (default).
    case ranked
    /// Grouped by document, documents by their best block, in line order.
    case document
}

/// Where evidence goes. Both placements keep it in user messages, never in
/// the system role.
public enum SyntextEvidencePlacement: String, Codable, Sendable {
    /// One user message: evidence, then the question (default). Works with
    /// chat templates that require strict role alternation.
    case withQuestion = "with_question"
    /// Evidence and the question as two consecutive user messages.
    case separateMessage = "separate_message"
}

/// Engine options. `nil` fields keep the defaults.
public struct SyntextContextEngineOptions: Encodable, Sendable {
    /// Model name recorded in receipts.
    public var model: String?
    /// Tokenizer name recorded in receipts when a token counter is given.
    public var tokenizer: String?
    /// Exact identifier search over a syntext index of the same files.
    public var syntext: SyntextIndexBinding?
    /// Entity-graph hops (default 3; 0 disables the graph walk).
    public var hops: Int?
    /// Passages retrieved before packing (default 24).
    public var maxResults: Int?
    /// Most evidence blocks in one prompt (default 10).
    public var maxBlocks: Int?
    /// Most blocks from one document (default 3).
    public var perDocCap: Int?
    /// Passages scoring below this fraction of the best are dropped
    /// (default 0.3).
    public var relevanceFloor: Double?
    /// Widen accepted passages into leftover budget (default true).
    public var expand: Bool?
    /// Block order (default `.ranked`).
    public var order: SyntextEvidenceOrder?
    /// Evidence placement (default `.withQuestion`).
    public var placement: SyntextEvidencePlacement?
    /// Append the evidence policy to the system prompt on turns with
    /// evidence (default true). Set false when your system prompt already
    /// includes `SyntextContextEngine.evidencePolicy`, so it stays constant
    /// across turns and the model's KV cache prefix is reused.
    public var appendPolicy: Bool?

    public init(
        model: String? = nil,
        tokenizer: String? = nil,
        syntext: SyntextIndexBinding? = nil,
        hops: Int? = nil,
        maxResults: Int? = nil,
        maxBlocks: Int? = nil,
        perDocCap: Int? = nil,
        relevanceFloor: Double? = nil,
        expand: Bool? = nil,
        order: SyntextEvidenceOrder? = nil,
        placement: SyntextEvidencePlacement? = nil,
        appendPolicy: Bool? = nil
    ) {
        self.model = model
        self.tokenizer = tokenizer
        self.syntext = syntext
        self.hops = hops
        self.maxResults = maxResults
        self.maxBlocks = maxBlocks
        self.perDocCap = perDocCap
        self.relevanceFloor = relevanceFloor
        self.expand = expand
        self.order = order
        self.placement = placement
        self.appendPolicy = appendPolicy
    }
}

/// A syntext index whose files a directory source mirrors under `label`.
public struct SyntextIndexBinding: Encodable, Sendable {
    public var indexDir: String
    public var repoRoot: String
    public var label: String
    public var scope: String?
    public var trust: SyntextTrust?

    public init(indexDir: String, repoRoot: String, label: String, scope: String? = nil, trust: SyntextTrust? = nil) {
        self.indexDir = indexDir
        self.repoRoot = repoRoot
        self.label = label
        self.scope = scope
        self.trust = trust
    }
}

// ── Wire DTOs ───────────────────────────────────────────────
struct PacketDto: Decodable {
    let blocks: [SyntextEvidenceBlock]
    let evidenceText: String
    let evidenceTokens: Int
    let budget: Int
    let lowestTrust: SyntextTrust?
    let receipt: SyntextReceipt
    let receiptJson: String

    var packet: SyntextEvidencePacket {
        SyntextEvidencePacket(
            blocks: blocks, evidenceText: evidenceText, evidenceTokens: evidenceTokens,
            budget: budget, lowestTrust: lowestTrust, receipt: receipt, receiptJSON: receiptJson)
    }
}

struct TurnDto: Decodable {
    let messages: [SyntextChatMessage]
    let packet: PacketDto
    let budget: SyntextBudget
    let historyKept: Int
    let historyDropped: Int
    let promptTokens: Int
}

struct PrepareRequestDto: Encodable {
    let question: String
    let evidenceTokens: Int
    let keptTurns: Int
    let scopes: [String]?
    let exclude: [String]
}

struct SearchRequestDto: Encodable {
    let question: String
    let maxResults: Int?
    let scopes: [String]?
    let exclude: [String]
}
