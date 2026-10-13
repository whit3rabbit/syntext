// Plumbing shared by the context wrappers: JSON decoding, callback
// trampolines, and error classification.

import Foundation
import CSyntext
import Syntext

/// Decode an owned JSON result string (already freed by `takeString`).
func decode<T: Decodable>(_ type: T.Type, _ json: String) throws -> T {
    try SyntextJSON.decoder.decode(T.self, from: Data(json.utf8))
}

/// Holds a token-counting closure for the C callback.
final class CounterBox {
    let count: @Sendable (String) -> Int
    init(_ count: @escaping @Sendable (String) -> Int) { self.count = count }
}

/// C trampoline into a `CounterBox` passed as userdata.
let countTrampoline: syntext_context_count_fn = { userdata, text, len in
    guard let userdata, let text else { return 0 }
    let box = Unmanaged<CounterBox>.fromOpaque(userdata).takeUnretainedValue()
    let s = String(decoding: UnsafeRawBufferPointer(start: text, count: len), as: UTF8.self)
    return UInt64(max(0, box.count(s)))
}

/// Holds a sync progress closure for the C callback (call-scoped).
final class ProgressBox {
    let report: (SyntextSyncProgress) -> Bool
    init(_ report: @escaping (SyntextSyncProgress) -> Bool) { self.report = report }
}

/// C trampoline into a `ProgressBox` passed as userdata.
let progressTrampoline: syntext_context_progress_fn = { userdata, observed, total, changed in
    guard let userdata else { return true }
    let box = Unmanaged<ProgressBox>.fromOpaque(userdata).takeUnretainedValue()
    return box.report(SyntextSyncProgress(
        observed: Int(clamping: observed), total: Int(clamping: total), changed: Int(clamping: changed)))
}

extension SyntextError {
    private var code: UInt32? {
        if case .indexError(let code, _) = self { return code }
        return nil
    }

    /// Another process holds the context store's writer lock; retry with
    /// backoff.
    public var isContextStoreLocked: Bool { code == UInt32(SYNTEXT_CONTEXT_ERR_LOCKED) }

    /// The system prompt and question alone exceed the model window.
    public var isWindowExceeded: Bool { code == UInt32(SYNTEXT_CONTEXT_ERR_WINDOW_EXCEEDED) }

    /// A sync was cancelled from its progress closure.
    public var isCancelled: Bool { code == UInt32(SYNTEXT_CONTEXT_ERR_CANCELLED) }
}

/// Version of the linked context engine.
public enum SyntextContextFFI {
    public static var version: String { String(cString: syntext_context_version()) }
}
