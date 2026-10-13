// Agent tools: let the model search again, read a source, and see what the
// corpus holds, through tool calls the host runs on its behalf.

import Foundation
import CSyntext
import Syntext

/// The shape of the tool definitions a host passes to its model.
public enum SyntextToolFormat: String, Sendable {
    /// `{"type": "function", "function": {name, description, parameters}}`.
    case openAI = "openai"
    /// Model Context Protocol: `{name, description, inputSchema}`.
    case mcp
}

/// A document range the model has already seen.
public struct SyntextShownRange: Codable, Sendable, Equatable {
    public let docKey: String
    /// First and last line (1-based).
    public let lines: [Int]

    public init(docKey: String, lines: [Int]) {
        self.docKey = docKey
        self.lines = lines
    }
}

/// Settings for tool calls. They come from the app, never from the model.
public struct SyntextToolOptions: Sendable, Equatable {
    /// Scopes the tools may read; `nil` uses the engine default.
    public var scopes: [String]?
    /// Tokens one result may use (at least 64).
    public var resultTokens: Int
    /// Most passages one search returns, whatever the model asks for.
    public var maxResults: Int
    /// Number of the first citation id in a result (`E<n>`).
    public var firstId: Int
    /// Ranges already in the conversation; searches skip what lies inside.
    public var shown: [SyntextShownRange]

    public init(
        scopes: [String]? = nil,
        resultTokens: Int = 1024,
        maxResults: Int = 6,
        firstId: Int = 1,
        shown: [SyntextShownRange] = []
    ) {
        self.scopes = scopes
        self.resultTokens = resultTokens
        self.maxResults = maxResults
        self.firstId = firstId
        self.shown = shown
    }

    /// Continue after `packet`: its blocks count as shown, and ids continue
    /// after its own.
    public mutating func record(_ packet: SyntextEvidencePacket) {
        record(blocks: packet.blocks)
    }

    /// Continue after a tool result.
    public mutating func record(_ result: SyntextToolResult) {
        record(blocks: result.blocks)
        firstId = max(firstId, result.nextId)
    }

    private mutating func record(blocks: [SyntextEvidenceBlock]) {
        for b in blocks {
            // A condensed block left lines out; a search may still need them.
            if !b.condensed {
                shown.append(SyntextShownRange(docKey: b.docKey, lines: b.lines))
            }
            if b.id.hasPrefix("E"), let n = Int(b.id.dropFirst()) {
                firstId = max(firstId, n + 1)
            }
        }
    }
}

/// The output of one tool call.
public struct SyntextToolResult: Decodable, Sendable, Equatable {
    /// The tool that ran.
    public let tool: String
    /// Text to return to the model as the tool's output.
    public let content: String
    /// Tokens of `content`.
    public let tokens: Int
    /// The call was malformed or named a source the tools cannot read;
    /// `content` tells the model why, so it can correct the call.
    public let isError: Bool
    /// Evidence blocks in `content` (search and read results).
    public let blocks: [SyntextEvidenceBlock]
    /// Least-trusted source in `content`; gate side-effecting tools on it.
    public let lowestTrust: SyntextTrust?
    /// One more than the highest citation id in `content`.
    public let nextId: Int
}

extension SyntextContextEngine {
    /// The tool definitions (`context_search`, `context_read`,
    /// `context_outline`) as a JSON array, to merge into the host's own
    /// tool list.
    public static func toolDefinitionsJSON(format: SyntextToolFormat = .openAI) throws -> String {
        let cFormat = CZString(format.rawValue)
        let json = try ffiCall { err in syntext_context_tool_definitions(cFormat?.ptr, err) }
        return takeString(json)
    }

    /// Run a tool call the model made. `arguments` is the call's JSON as
    /// the model wrote it. A malformed call comes back with `isError` and a
    /// message for the model; only failures of the store, or a
    /// `resultTokens` below 64, throw.
    public func callTool(
        name: String,
        arguments: String,
        options: SyntextToolOptions = .init()
    ) throws -> SyntextToolResult {
        let request = ToolCallDto(
            name: name, arguments: arguments, scopes: options.scopes,
            resultTokens: options.resultTokens, maxResults: options.maxResults,
            firstId: options.firstId, shown: options.shown)
        let cRequest = try CZString(encodeJSON(request))
        let json = try ffiCall { err in syntext_context_tool_call(handle, cRequest?.ptr, err) }
        return try decode(SyntextToolResult.self, takeString(json))
    }

    /// `callTool` off the calling thread.
    public func callToolAsync(
        name: String,
        arguments: String,
        options: SyntextToolOptions = .init()
    ) async throws -> SyntextToolResult {
        try await Task.detached(priority: .userInitiated) {
            try self.callTool(name: name, arguments: arguments, options: options)
        }.value
    }
}

struct ToolCallDto: Encodable {
    let name: String
    let arguments: String
    let scopes: [String]?
    let resultTokens: Int
    let maxResults: Int
    let firstId: Int
    let shown: [SyntextShownRange]
}
