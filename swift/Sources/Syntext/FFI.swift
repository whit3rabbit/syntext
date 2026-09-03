// Thin, safe wrappers over the raw C ABI (CSyntext). All pointer/lifetime
// handling lives here; the public API layers never touch raw pointers.

import Foundation
import CSyntext

/// Runs `body` with a fresh error out-parameter and materializes any error
/// the C side stored into a thrown `SyntextError`, freeing the handle.
func ffiCall<T>(_ body: (UnsafeMutablePointer<OpaquePointer?>) throws -> T) throws -> T {
    var err: OpaquePointer? = nil
    let result = try body(&err)
    if let e = err {
        let code = syntext_error_code(e)
        let message = String(cString: syntext_error_message(e))
        syntext_error_free(e)
        throw SyntextError.indexError(code: code, message: message)
    }
    return result
}

/// Reads an owned JSON string returned by the C ABI and frees it.
func takeString(_ p: UnsafeMutablePointer<CChar>?) -> String {
    guard let p else { return "" }
    defer { syntext_string_free(p) }
    return String(cString: p)
}

/// An owned NUL-terminated C string. `nil` instance argument means NULL.
/// Deallocation happens in `deinit`, so the pointer stays valid for the whole
/// enclosing FFI call without nested `withCString` pyramids.
final class CZString {
    private let buffer: UnsafeMutablePointer<CChar>
    private let isAllocated: Bool

    init?(_ s: String?) {
        guard let s else { return nil }
        let units = Array(s.utf8CString) // includes the terminating NUL
        // The Rust side reads this back with `CStr::from_ptr`, which stops at
        // the first NUL: a string with an embedded NUL would silently
        // truncate there instead of erroring. Reject it here so the caller
        // sees a NULL argument (and an FFI invalid-argument error) instead of
        // truncated content crossing the boundary unnoticed.
        guard !units.dropLast().contains(0) else { return nil }
        buffer = units.withUnsafeBufferPointer { src in
            let dst = UnsafeMutablePointer<CChar>.allocate(capacity: src.count)
            dst.initialize(from: src.baseAddress!, count: src.count)
            return dst
        }
        isAllocated = true
    }

    /// NULL when constructed from a nil string, else the C string pointer.
    var ptr: UnsafePointer<CChar>? { UnsafePointer(buffer) }

    deinit {
        if isAllocated {
            buffer.deallocate()
        }
    }
}

/// Shared snake_case JSON coder pair (Rust DTOs are snake_case on the wire).
enum SyntextJSON {
    static let decoder: JSONDecoder = {
        let d = JSONDecoder()
        d.keyDecodingStrategy = .convertFromSnakeCase
        return d
    }()

    static let encoder: JSONEncoder = {
        let e = JSONEncoder()
        e.keyEncodingStrategy = .convertToSnakeCase
        return e
    }()
}

/// Crate version reported by the linked staticlib.
public enum SyntextFFI {
    /// Version string of the compiled Rust library.
    public static var version: String {
        String(cString: syntext_version())
    }
}
