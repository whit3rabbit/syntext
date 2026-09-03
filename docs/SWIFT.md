# Swift Bindings

Use syntext from Swift (macOS 12+) to index source-code projects on disk and to search in-memory content such as chat transcripts. The bindings are a hand-written C ABI (`src/ffi/`, `ffi` Cargo feature) compiled into a static library, shipped as the `SyntextFFI.xcframework` binary target of the Swift package in [`swift/`](../swift/), and wrapped by a thin Swift layer.

- Package: `Syntext` (products: `Syntext` library)
- Platforms: macOS (arm64 + x86_64 in one universal slice)
- Zero additional Rust or Swift dependencies

## Adding the package

Xcode: File > Add Package Dependencies, enter `https://github.com/whit3rabbit/syntext`, and add the `Syntext` library to your target.

Package.swift:

```swift
.package(url: "https://github.com/whit3rabbit/syntext", branch: "main")
```

Pin by branch (`main`) or by a `swift-vX.Y.Z` tag. Do not pin an exact `vX.Y.Z` tag: that tag's `swift/Package.swift` necessarily references the release zip of the *previous* version, because the zip for `X.Y.Z` is published by the release triggered from that same tag. (The fully tag-exact flow, a prepare-PR that records the checksum before tagging, is deliberate future work.)

Local development: run `./swift/Scripts/build-xcframework.sh` first. It builds the xcframework into `swift/build/`, and `swift/Package.swift` prefers that local copy over the pinned release zip, so `swift test` never needs an unreleased artifact.

## Searching a project (on-disk index)

```swift
import Syntext

// Build once (walks repoRoot, gitignore-aware) into an index directory.
let index = try SyntextIndex.build(indexDir: "\(cacheDir)/myproject.syntext",
                                   repoRoot: "/path/to/project")

// Search. Blocking; call off the main thread.
let matches = try index.search("parse_query", options: .init(caseInsensitive: true))
for m in matches {
    print("\(m.path):\(m.lineNumber): \(m.lineContent)")   // lossy display string
    print(m.matchText())                                   // exact matched bytes
}

// Keep it fresh without a full rebuild:
try index.notifyChange("/path/to/project/src/new_file.rs")  // absolute path
try index.commitBatch()                                     // atomic snapshot swap
let result = try index.searchFresh("parse_query")           // bounded git update + search
```

`searchFresh`/`updateFromGit` shell out to `git`, so they need `git` on `PATH` (see Sandbox notes).

## Searching chats (in-memory index)

```swift
let chats = try SyntextChatIndex()
try chats.add("chats/42/msg-7", content: "the needle in message seven")
try chats.add("chats/42/msg-8", content: Data(...))   // Data overload: bytes may be non-UTF-8
try chats.commit()                                     // publish; O(total content)

let hits = try chats.search("needle")
try chats.remove("chats/42/msg-7")
try chats.commit()                                     // removal applies atomically
```

- Document ids double as index paths: non-empty, no leading `/`, no `..` (rejected with error code 6).
- Same-id `add` replaces the document.
- Nothing is searchable until `commit()`; a commit is an atomic snapshot swap, so searches in flight keep seeing the old snapshot.
- `commit()` rebuilds the whole snapshot: O(total indexed content), right-sized for thousands of small documents. For large corpora use `SyntextIndex`.
- Content that looks binary (a NUL byte in the first 8 KiB) is silently skipped at commit, exactly like on-disk ingestion.

## Search results and byte-exactness

Every match carries both a lossy UTF-8 rendering (`lineContent`, for display) and the exact bytes (`lineContentBytes`, base64-decoded). `submatchStart`, `submatchEnd`, and `byteOffset` are defined only against `lineContentBytes`; a lossy string can shift byte offsets (each U+FFFD replacement is 3 bytes). Always compute highlights from the bytes.

## Options, config, limits

`SyntextSearchOptions` (snake_case JSON over the ABI; unknown fields ignored, so the Rust side can grow):

```jsonc
{ "path_filter": null, "file_type": null, "exclude_type": null,
  "file_types": [], "exclude_types": [], "max_results": null,
  "case_insensitive": false, "verify_pattern": null,
  "skip_line_content": false, "deterministic": false }
```

`max_results`: absent **or `0`** uses the FFI default 10,000; other explicit values are capped at 1,000,000 (no lower-bound clamping: any value from 1 up to the cap passes through unchanged). A negative value fails JSON decoding (the field is unsigned) and surfaces as error code 100.

`SyntextConfig` (build/open): `{ "max_file_size": 10485760, "max_segments": 10, "strict_permissions": true, "verify_on_open": false }`.

`SyntextUpdateLimits` (searchFresh/updateFromGit): `{ "max_files": null, "budget_ms": null }`; a nil `limits` argument uses the CLI defaults (200 files / 150 ms), while explicit nil fields mean no limit.

## Errors

`SyntextError.indexError(code:message:)` with stable, append-only codes (never renumbered; see `swift/Sources/CSyntext/include/syntext.h`):

| Code | Meaning |
|---|---|
| 0 | OK |
| 1 | I/O error |
| 2 | no index at indexDir (build first) |
| 3 | invalid regex |
| 4 | corrupt index (rebuild) |
| 5 | query too broad (narrow it) |
| 6 | path outside repo / bad chat document id |
| 7 | file too large |
| 8 | lock conflict, **retryable** |
| 9 | overlay full (run a full rebuild) |
| 10 | doc-id overflow |
| 100 | invalid argument (NULL handle, bad JSON, non-UTF-8 input) |
| 101 | Rust panic caught at the boundary |
| 200 | unknown error variant (newer library) |

`LockConflict` (8): another process holds a conflicting flock on the index directory. Retry with bounded exponential backoff (`isRetryableLockConflict`). Note it can also indicate a kernel lock-resource failure (for example `ENOLCK` on macOS under heavy process churn), not just contention.

## Threading

- Both index types are `@unchecked Sendable`: the underlying Rust types are `Send + Sync` (statically asserted in the Rust source) and the Swift classes hold only the opaque handle.
- All calls are blocking. Call them off the main thread.
- The first parallel operation initializes the rayon global thread pool (roughly one worker per core). This is normal; it does not contend with Swift concurrency.
- Searches are isolated from concurrent commits by snapshot swap (`ArcSwap` on the Rust side).

## Sandbox and environment notes

- `updateFromGit` resolves and spawns `git` and propagates a failure to resolve/spawn it as error code 1 (I/O). `searchFresh` does not: it treats a `git` detection failure the same as "no changes" (`UpdateOutcome.noChanges`, `detectElapsedMs: 0`) and always proceeds to search the existing index, so a missing `git` never surfaces as an error from `searchFresh` (check the returned `UpdateOutcome` if you need to detect a stale index in a sandboxed environment). Sandboxed macOS apps with a stripped environment commonly hit this with a missing `git`. Extend `PATH` (for example with `/usr/bin:/usr/local/bin` via the app's entitlements or a bundled git) or stick to `notifyChange`/`commitBatch`.
- The library reads `SYNTEXT_VERIFY_ON_OPEN=1` (full checksum at open). `SYNTEXT_NO_ASYNC_UPDATE` affects only the CLI, not these APIs.
- Logging goes through the Rust `log` facade and is silent by default.

## Limitations

- **One Rust static library per application.** The xcframework exports every Rust symbol (std, regex, rayon, ...); linking a second Rust staticlib into the same binary fails at link time.
- `commit()` on `SyntextChatIndex` is O(total content) (see above).
- The initial `swift/Package.swift` URL pin points at the latest published release; before the first release carrying the `ffi` feature, only the local build path resolves.
- iOS is not built yet; adding slices to the xcframework is the follow-up path.

## Rebuilding the xcframework

```sh
./swift/Scripts/build-xcframework.sh
# output: swift/build/SyntextFFI.xcframework
#         swift/build/syntext-swift-<version>.xcframework.zip (+ .sha256)
```

The script builds the Rust staticlib for `aarch64-apple-darwin` and `x86_64-apple-darwin` (on-demand `cargo rustc --crate-type staticlib`; the manifest crate-type stays `["cdylib","rlib"]` so ordinary builds are unaffected), merges them with `lipo` into one universal library (static-library xcframework slices are per-platform, not per-arch), and packages the checksummed zip that the release workflow publishes.

## C ABI reference

The full ABI (entry points, ownership rules, error codes) is documented in the hand-written header `swift/Sources/CSyntext/include/syntext.h`, which mirrors `src/ffi/` one to one. Rust-side integration tests: `tests/integration/ffi.rs` (`cargo test --features ffi`).
