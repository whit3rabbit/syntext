// Public value types: Codable mirrors of the Rust DTOs plus the error type.

import Foundation
import CSyntext

/// Error surfaced by the syntext FFI. `code` is one of the stable
/// `SYNTEXT_ERR_*` values from syntext.h.
public enum SyntextError: Error, Sendable {
    case indexError(code: UInt32, message: String)

    /// `SYNTEXT_ERR_LOCK_CONFLICT` (8): another process holds a conflicting
    /// flock on the index directory. Retryable with bounded exponential
    /// backoff. Note this can also indicate a kernel lock-resource failure
    /// (e.g. `ENOLCK` on macOS under heavy process churn), not just contention.
    public var isRetryableLockConflict: Bool {
        if case .indexError(let code, _) = self {
            return code == UInt32(SYNTEXT_ERR_LOCK_CONFLICT)
        }
        return false
    }

    /// The error message from the Rust side (nil never happens today; kept
    /// optional so callers can bind without a switch).
    public var message: String? {
        if case .indexError(_, let m) = self { return m }
        return nil
    }
}

// ── Wire DTOs (internal) ─────────────────────────────────────
struct ContextLineDto: Decodable {
    let lineNumber: Int
    let lineContent: String
    let isMatch: Bool
}

struct MatchDto: Decodable {
    let path: String
    let lineNumber: Int
    let lineContent: String
    let lineContentB64: String
    let byteOffset: UInt64
    let submatchStart: Int
    let submatchEnd: Int
    let context: [ContextLineDto]?
}

struct StatsDto: Decodable {
    let totalDocuments: Int
    let totalSegments: Int
    let totalGrams: Int
    let indexSizeBytes: UInt64
    let baseCommit: String?
    let overlayGenerations: Int
    let pendingEdits: Int
}

/// Tolerant decoding of the tagged `UpdateOutcomeDto`: unknown kinds decode
/// to `.unknown` instead of throwing, so a newer Rust side stays usable.
struct UpdateOutcomeDto: Decodable {
    let kind: String
    let files: Int?
    let skipped: Int?
    let detectElapsedMs: UInt64?
    let filesBehindEstimate: Int?
    let filesBehind: Int?
}

struct SearchFreshDto: Decodable {
    let matches: [MatchDto]
    let updateOutcome: UpdateOutcomeDto
}

// ── Public results ───────────────────────────────────────────
/// One context line surrounding a match, or the matched line itself.
public struct SyntextContextLine: Sendable, Equatable, Codable {
    /// 1-based line number.
    public let lineNumber: Int
    /// Lossy UTF-8 rendering of the line.
    public let lineContent: String
    /// True if this line is the matching line; false if it is a context line.
    public let isMatch: Bool

    public init(lineNumber: Int, lineContent: String, isMatch: Bool) {
        self.lineNumber = lineNumber
        self.lineContent = lineContent
        self.isMatch = isMatch
    }

    init(dto: ContextLineDto) {
        self.lineNumber = dto.lineNumber
        self.lineContent = dto.lineContent
        self.isMatch = dto.isMatch
    }
}

/// One line-level search match.
///
/// `lineContent` is a lossy UTF-8 rendering for display. `lineContentBytes`
/// holds the exact bytes; `submatchStart`, `submatchEnd`, and `byteOffset`
/// are defined ONLY against those bytes (a lossy string can shift offsets).
public struct SyntextSearchMatch: Sendable, Equatable {
    /// Repo-relative path (native index) or document id (chat index).
    public let path: String
    /// 1-based line number.
    public let lineNumber: Int
    /// Lossy UTF-8 rendering of the matched line (display only).
    public let lineContent: String
    /// Exact bytes of the matched line; submatch offsets index this.
    public let lineContentBytes: Data
    /// Byte offset of the first match within the document.
    public let byteOffset: UInt64
    /// Byte offset of the match start within `lineContentBytes`.
    public let submatchStart: Int
    /// Exclusive byte offset of the match end within `lineContentBytes`.
    public let submatchEnd: Int
    /// Surrounding context lines (ordered by line number) if context was requested.
    public let context: [SyntextContextLine]

    /// Preceding context lines.
    public var beforeContext: [SyntextContextLine] {
        context.filter { !$0.isMatch && $0.lineNumber < lineNumber }
    }

    /// Following context lines.
    public var afterContext: [SyntextContextLine] {
        context.filter { !$0.isMatch && $0.lineNumber > lineNumber }
    }

    /// The matched substring, lossily decoded from the exact bytes.
    public func matchText() -> String {
        guard submatchStart >= 0, submatchEnd >= submatchStart,
              submatchEnd <= lineContentBytes.count else { return "" }
        let sub = lineContentBytes.subdata(in: submatchStart..<submatchEnd)
        return String(decoding: sub, as: UTF8.self)
    }

    init(dto: MatchDto) {
        path = dto.path
        lineNumber = dto.lineNumber
        lineContent = dto.lineContent
        lineContentBytes = Data(base64Encoded: dto.lineContentB64) ?? Data()
        byteOffset = dto.byteOffset
        submatchStart = dto.submatchStart
        submatchEnd = dto.submatchEnd
        context = dto.context?.map(SyntextContextLine.init(dto:)) ?? []
    }
}

/// Counters reported by `SyntextIndex.stats()`.
public struct SyntextStats: Sendable, Equatable {
    public let totalDocuments: Int
    public let totalSegments: Int
    public let totalGrams: Int
    public let indexSizeBytes: UInt64
    public let baseCommit: String?
    public let overlayGenerations: Int
    public let pendingEdits: Int

    init(dto: StatsDto) {
        totalDocuments = dto.totalDocuments
        totalSegments = dto.totalSegments
        totalGrams = dto.totalGrams
        indexSizeBytes = dto.indexSizeBytes
        baseCommit = dto.baseCommit
        overlayGenerations = dto.overlayGenerations
        pendingEdits = dto.pendingEdits
    }
}

/// Outcome of a bounded git update (`updateFromGit` / `searchFresh`).
public enum SyntextUpdateOutcome: Sendable, Equatable {
    /// `files` change notifications applied; `skipped` counts files left stale.
    case updated(files: Int, skipped: Int, detectElapsedMs: UInt64)
    /// Nothing changed since the last build.
    case noChanges(detectElapsedMs: UInt64)
    /// Detection budget exhausted; index not updated.
    case budgetExceeded(filesBehindEstimate: Int, detectElapsedMs: UInt64)
    /// Change set exceeded `maxFiles`; index not updated.
    case tooManyFiles(filesBehind: Int, detectElapsedMs: UInt64)
    /// Applying changes would exceed the overlay cap; index not updated.
    case overlayFull(filesBehind: Int, detectElapsedMs: UInt64)
    /// A kind this Swift version does not know (newer Rust side).
    case unknown

    init(dto: UpdateOutcomeDto) {
        let ms = dto.detectElapsedMs ?? 0
        switch dto.kind {
        case "updated":
            self = .updated(files: dto.files ?? 0, skipped: dto.skipped ?? 0, detectElapsedMs: ms)
        case "no_changes":
            self = .noChanges(detectElapsedMs: ms)
        case "budget_exceeded":
            self = .budgetExceeded(filesBehindEstimate: dto.filesBehindEstimate ?? 0, detectElapsedMs: ms)
        case "too_many_files":
            self = .tooManyFiles(filesBehind: dto.filesBehind ?? 0, detectElapsedMs: ms)
        case "overlay_full":
            self = .overlayFull(filesBehind: dto.filesBehind ?? 0, detectElapsedMs: ms)
        default:
            self = .unknown
        }
    }
}

/// `searchFresh` result: matches plus what the bounded update did.
public struct SyntextSearchResult: Sendable {
    public let matches: [SyntextSearchMatch]
    public let outcome: SyntextUpdateOutcome
}

// ── Inputs ───────────────────────────────────────────────────
/// Search options (subset of Rust `SearchOptions` exposed over the FFI).
/// Absent `maxResults` means the FFI default (10,000; hard cap 1,000,000).
public struct SyntextSearchOptions: Sendable, Encodable {
    public var pathFilter: String?
    public var fileType: String?
    public var excludeType: String?
    public var fileTypes: [String]
    public var excludeTypes: [String]
    public var maxResults: Int?
    public var caseInsensitive: Bool
    public var verifyPattern: String?
    public var skipLineContent: Bool
    public var deterministic: Bool
    /// Treat the pattern as a literal string instead of a regular expression (`-F`).
    public var fixedStrings: Bool
    /// Only show matches surrounded by word boundaries (`-w`).
    public var wordRegexp: Bool
    /// Only show matches surrounded by line boundaries (`-x`).
    public var lineRegexp: Bool
    /// Number of context lines to capture before each match (`-B`).
    public var beforeContext: Int?
    /// Number of context lines to capture after each match (`-A`).
    public var afterContext: Int?

    public init(
        pathFilter: String? = nil,
        fileType: String? = nil,
        excludeType: String? = nil,
        fileTypes: [String] = [],
        excludeTypes: [String] = [],
        maxResults: Int? = nil,
        caseInsensitive: Bool = false,
        verifyPattern: String? = nil,
        skipLineContent: Bool = false,
        deterministic: Bool = false,
        fixedStrings: Bool = false,
        wordRegexp: Bool = false,
        lineRegexp: Bool = false,
        context: Int? = nil,
        beforeContext: Int? = nil,
        afterContext: Int? = nil
    ) {
        self.pathFilter = pathFilter
        self.fileType = fileType
        self.excludeType = excludeType
        self.fileTypes = fileTypes
        self.excludeTypes = excludeTypes
        self.maxResults = maxResults
        self.caseInsensitive = caseInsensitive
        self.verifyPattern = verifyPattern
        self.skipLineContent = skipLineContent
        self.deterministic = deterministic
        self.fixedStrings = fixedStrings
        self.wordRegexp = wordRegexp
        self.lineRegexp = lineRegexp
        self.beforeContext = beforeContext ?? context
        self.afterContext = afterContext ?? context
    }
}

extension Collection where Element == SyntextSearchMatch {
    /// Render matches in standard ripgrep format (`path:line:content`),
    /// using `-` for context lines, `:` for matches, and `--` separator between non-contiguous blocks.
    public func formattedGrepOutput(contextSeparator: String = "--") -> String {
        var lines: [String] = []
        var lastPath: String? = nil
        var lastLineNumber: Int? = nil

        for m in self {
            if !m.context.isEmpty {
                for c in m.context {
                    if let lp = lastPath, let ll = lastLineNumber {
                        if lp == m.path && c.lineNumber == ll {
                            // Avoid duplicating identical line from overlapping context
                            continue
                        }
                        if lp != m.path || c.lineNumber > ll + 1 {
                            if !lines.isEmpty {
                                lines.append(contextSeparator)
                            }
                        }
                    }
                    let sep = c.isMatch ? ":" : "-"
                    lines.append("\(m.path)\(sep)\(c.lineNumber)\(sep)\(c.lineContent)")
                    lastPath = m.path
                    lastLineNumber = c.lineNumber
                }
            } else {
                if let lp = lastPath, let ll = lastLineNumber {
                    if lp != m.path || m.lineNumber > ll + 1 {
                        if !lines.isEmpty {
                            lines.append(contextSeparator)
                        }
                    }
                }
                lines.append("\(m.path):\(m.lineNumber):\(m.lineContent)")
                lastPath = m.path
                lastLineNumber = m.lineNumber
            }
        }
        return lines.joined(separator: "\n")
    }
}

/// Index build/open configuration (subset of Rust `Config`).
public struct SyntextConfig: Sendable, Encodable {
    /// Maximum file size to index, bytes (default 10 MB).
    public var maxFileSize: Int?
    /// Maximum segments before a merge is triggered (default 10).
    public var maxSegments: Int?
    /// Reject index dirs with group/other permission bits (unix, default true).
    public var strictPermissions: Bool?
    /// Fully checksum each segment at open (default false; O(postings) I/O).
    public var verifyOnOpen: Bool?

    public init(
        maxFileSize: Int? = nil,
        maxSegments: Int? = nil,
        strictPermissions: Bool? = nil,
        verifyOnOpen: Bool? = nil
    ) {
        self.maxFileSize = maxFileSize
        self.maxSegments = maxSegments
        self.strictPermissions = strictPermissions
        self.verifyOnOpen = verifyOnOpen
    }
}

/// Bounds for a bounded git update. Both nil = no limit. Passing `nil` as the
/// `limits` argument uses the CLI defaults (200 files / 150 ms) instead.
public struct SyntextUpdateLimits: Sendable, Encodable {
    public var maxFiles: Int?
    public var budgetMs: UInt64?

    public init(maxFiles: Int? = nil, budgetMs: UInt64? = nil) {
        self.maxFiles = maxFiles
        self.budgetMs = budgetMs
    }
}
