// Public models of stores, sources, and sync. Wire JSON is snake_case; the
// shared coders convert to and from these camelCase names.

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
