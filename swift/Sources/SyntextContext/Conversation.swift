// SyntextConversation: one conversation's durable, searchable memory.

import Foundation
import CSyntext
import Syntext

/// Where a conversation's turns persist.
public enum SyntextMemoryMode: Sendable, Equatable {
    /// Nothing persists.
    case off
    /// Shared across sessions of one project (scope `project:<id>`).
    case project(String)
    /// Private to one application (scope `app:<id>`).
    case app(String)

    var json: String {
        func object(_ key: String, _ id: String) -> String {
            let data = (try? JSONSerialization.data(withJSONObject: [key: id])) ?? Data("{}".utf8)
            return String(decoding: data, as: UTF8.self)
        }
        switch self {
        case .off: return "\"off\""
        case .project(let id): return object("project", id)
        case .app(let id): return object("app", id)
        }
    }
}

/// One conversation's memory. Every recorded turn is durable before
/// `record` returns and immediately searchable. `SyntextContextEngine`
/// keeps recent turns verbatim in the prompt and brings older ones back as
/// cited evidence when a question needs them. Recording never waits on a
/// running sync or compaction.
public final class SyntextConversation: @unchecked Sendable {
    let handle: OpaquePointer?

    /// Open (or resume) conversation `session` in `store`.
    public init(store: SyntextContextStore, session: String, mode: SyntextMemoryMode) throws {
        let cSession = CZString(session)
        let cMode = CZString(mode.json)
        handle = try ffiCall { err in
            syntext_context_memory_open(store.handle, cSession?.ptr, cMode?.ptr, err)
        }
    }

    deinit {
        syntext_context_memory_free(handle)
    }

    /// Durably record one turn.
    public func record(_ role: SyntextChatRole, _ text: String) throws {
        let cRole = CZString(role.rawValue)
        let cText = CZString(text)
        _ = try ffiCall { err in syntext_context_memory_record(handle, cRole?.ptr, cText?.ptr, err) }
    }

    /// Record a question and the model's answer.
    public func recordExchange(question: String, answer: String) throws {
        try record(.user, question)
        try record(.assistant, answer)
    }

    /// Delete every persisted turn of this conversation and clear its
    /// history, for a chat the user deleted or rewrote (record the kept
    /// turns again afterwards). Journaled turns leave the disk now; compacted
    /// ones at the next `store.compact(force: true)`. Other handles open on
    /// the same session keep their in-process history; reopen them. Returns
    /// how many turns were removed. Idempotent.
    @discardableResult
    public func forget() throws -> Int {
        var removed: UInt64 = 0
        _ = try ffiCall { err in syntext_context_memory_forget(handle, &removed, err) }
        return Int(removed)
    }

    /// Every turn, oldest first.
    public func history() throws -> [SyntextChatMessage] {
        let json = try ffiCall { err in syntext_context_memory_history(handle, err) }
        return try decode([SyntextChatMessage].self, takeString(json))
    }
}
