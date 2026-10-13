// Public models of the context engine. Wire JSON is snake_case; the shared
// coders convert to and from these camelCase names.

import Foundation
import CSyntext
import Syntext

/// How much the host trusts a source, least to most. Evidence is reference
/// data at every level: trust is for host policy, such as asking before a
/// side-effecting tool runs in a turn whose evidence includes `.external`.
public enum SyntextTrust: String, Codable, Sendable, Comparable {
    /// Downloaded pages, tool output, third-party files.
    case external
    /// Past conversation turns (they may quote external content).
    case memory
    /// Files from the user's own workspace.
    case workspace
    /// Material curated by the host.
    case curated

    private var rank: Int {
        switch self {
        case .external: return 0
        case .memory: return 1
        case .workspace: return 2
        case .curated: return 3
        }
    }

    public static func < (a: SyntextTrust, b: SyntextTrust) -> Bool { a.rank < b.rank }
}

/// How much of the entity graph a new store extracts.
public enum SyntextGraphMode: String, Codable, Sendable {
    /// Code and prose facts (default).
    case full
    /// Code facts only.
    case code
    /// No graph.
    case off
}

/// Options for opening a store. `nil` fields keep the engine defaults.
public struct SyntextContextStoreOptions: Encodable, Sendable {
    /// Open without the writer lock (writes fail; use `reload()`).
    public var readOnly: Bool
    /// Fail instead of creating a store when none exists.
    public var mustExist: Bool
    /// Fsync writes (default true).
    public var sync: Bool?
    /// Source bytes compiled per segment; bounds write memory (default 32 MiB).
    public var batchBytes: Int?
    /// Merge small segments past this many (default 8).
    public var maxSegments: Int?
    /// Rewrite the store when this share of documents is dead (default 0.3).
    public var maxDeadRatio: Double?
    /// Turns kept live before they are compacted (default 32).
    public var memoryFlushTurns: Int?
    /// Checksum every section at open (default false).
    public var verifyOnOpen: Bool?
    /// Graph extraction for a new store.
    public var graph: SyntextGraphMode?

    public init(
        readOnly: Bool = false,
        mustExist: Bool = false,
        sync: Bool? = nil,
        batchBytes: Int? = nil,
        maxSegments: Int? = nil,
        maxDeadRatio: Double? = nil,
        memoryFlushTurns: Int? = nil,
        verifyOnOpen: Bool? = nil,
        graph: SyntextGraphMode? = nil
    ) {
        self.readOnly = readOnly
        self.mustExist = mustExist
        self.sync = sync
        self.batchBytes = batchBytes
        self.maxSegments = maxSegments
        self.maxDeadRatio = maxDeadRatio
        self.memoryFlushTurns = memoryFlushTurns
        self.verifyOnOpen = verifyOnOpen
        self.graph = graph
    }
}

/// A directory mirrored into a store under `label` (keys `<label>:<path>`).
public struct SyntextDirectorySource: Encodable, Sendable {
    /// Key prefix, `[a-z0-9_-]{1,64}`.
    public var label: String
    /// Root directory.
    public var root: String
    /// Visibility scope (default `"default"`).
    public var scope: String?
    /// Trust of the files (default `.workspace`).
    public var trust: SyntextTrust?
    /// Files larger than this are skipped (default 2 MiB).
    public var maxFileBytes: UInt64?
    /// Also index nested git checkouts.
    public var includeNestedCheckouts: Bool
    /// Relative path prefixes to leave out.
    public var exclude: [String]

    public init(
        label: String,
        root: String,
        scope: String? = nil,
        trust: SyntextTrust? = nil,
        maxFileBytes: UInt64? = nil,
        includeNestedCheckouts: Bool = false,
        exclude: [String] = []
    ) {
        self.label = label
        self.root = root
        self.scope = scope
        self.trust = trust
        self.maxFileBytes = maxFileBytes
        self.includeNestedCheckouts = includeNestedCheckouts
        self.exclude = exclude
    }
}

/// Progress of a sync, reported after each chunk of observed paths.
public struct SyntextSyncProgress: Sendable, Equatable {
    /// Paths checked so far.
    public let observed: Int
    /// Paths this sync checks.
    public let total: Int
    /// Changed files queued for compilation so far.
    public let changed: Int
}

/// What a sync changed.
public struct SyntextSyncReport: Decodable, Sendable, Equatable {
    public let generation: UInt64
    public let scanned: Int
    public let added: Int
    public let modified: Int
    public let removed: Int
    public let skipped: Int
    public let uncertain: Int
}

/// A host document (a note, or external content keyed by URI).
public struct SyntextContextDocument: Encodable, Sendable {
    /// Note id (key `note:<id>`), or a URI when `external` (key `ext:<uri>`).
    public var id: String
    public var text: String
    public var external: Bool
    public var title: String?
    public var trust: SyntextTrust?
    public var scope: String?
    /// Unix seconds.
    public var timestamp: Int64?

    public init(
        id: String,
        text: String,
        external: Bool = false,
        title: String? = nil,
        trust: SyntextTrust? = nil,
        scope: String? = nil,
        timestamp: Int64? = nil
    ) {
        self.id = id
        self.text = text
        self.external = external
        self.title = title
        self.trust = trust
        self.scope = scope
        self.timestamp = timestamp
    }
}

/// Outcome of an upsert or removal.
public struct SyntextIngestReport: Decodable, Sendable, Equatable {
    public let generation: UInt64
    public let upserted: Int
    public let removed: Int
    public let passages: Int
    public let segments: Int
    /// Documents that could not be compiled, as `[key, reason]` pairs.
    public let skipped: [[String]]
}

/// Outcome of a compaction.
public struct SyntextCompactReport: Decodable, Sendable, Equatable {
    public let generation: UInt64
    public let segmentsBefore: Int
    public let segmentsAfter: Int
    public let documents: Int
    public let deadDropped: UInt64
}

/// Store statistics.
public struct SyntextStoreStats: Decodable, Sendable, Equatable {
    public let generation: UInt64
    public let documents: Int
    public let passages: UInt64
    public let segments: Int
    public let liveTurns: Int
    public let bytes: UInt64
    public let deadDocuments: UInt64
    public let entities: UInt64
    public let staleSegments: Int
}

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

/// Engine options. `nil` fields keep the defaults.
public struct SyntextContextEngineOptions: Encodable, Sendable {
    /// Model name recorded in receipts.
    public var model: String?
    /// Tokenizer name recorded in receipts when a token counter is given.
    public var tokenizer: String?
    /// Exact identifier search over a syntext index of the same files.
    public var syntext: SyntextIndexBinding?
    public var hops: Int?
    public var maxResults: Int?
    public var maxBlocks: Int?
    public var perDocCap: Int?
    public var relevanceFloor: Double?
    public var expand: Bool?
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
    let scopes: [String]?
    let exclude: [String]
}

struct SearchRequestDto: Encodable {
    let question: String
    let maxResults: Int?
    let scopes: [String]?
    let exclude: [String]
}
