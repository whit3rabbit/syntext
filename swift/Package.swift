// swift-tools-version:5.9
import PackageDescription
import Foundation

// Binary-target resolution, in priority order:
//
// 1. Dev/CI: Scripts/build-xcframework.sh drops a local xcframework at
//    build/SyntextFFI.xcframework; when present it is preferred, so
//    `swift test` never needs an unreleased artifact.
// 2. Consumers: the pinned release zip. The Swift prepare workflow stages
//    a release's zip and pins it here BEFORE the vX.Y.Z tag is cut, so that
//    tag pins its own zip and `exact: "X.Y.Z"` works. Tags through v2.7.0
//    were made by the legacy flow and pin the PREVIOUS release's zip: for
//    those, pin the `swift-vX.Y.Z` commit by revision. See docs/SWIFT.md.
let hasLocalFFI = FileManager.default.fileExists(atPath: "build/SyntextFFI.xcframework")
let ffiTarget: Target = hasLocalFFI
    ? .binaryTarget(name: "SyntextFFI", path: "build/SyntextFFI.xcframework")
    : .binaryTarget(
        name: "SyntextFFI",
        // Placeholder until the first release carrying the `ffi` feature
        // (v2.2.0); the pin job replaces it.
        url: "https://github.com/whit3rabbit/syntext/releases/download/v2.7.1/syntext-swift-2.7.1.xcframework.zip",
        checksum: "b1b2faf4ee3cb42f118c2172114c78ed846b94ac1eedf668d93e54f9cd33c477")

let package = Package(
    name: "Syntext",
    platforms: [.macOS(.v12)],
    products: [
        .library(name: "Syntext", targets: ["Syntext"]),
        // The context engine (docs/CONTEXT.md). A separate product: its C
        // symbols ship in the same xcframework from the release that adds
        // it, so apps using only `Syntext` never depend on them.
        .library(name: "SyntextContext", targets: ["SyntextContext"]),
    ],
    targets: [
        ffiTarget,
        .target(
            name: "CSyntext",
            dependencies: ["SyntextFFI"],
            publicHeadersPath: "include"),
        .target(
            name: "Syntext",
            dependencies: ["CSyntext", "SyntextFFI"]),
        .target(
            name: "SyntextContext",
            dependencies: ["Syntext", "CSyntext", "SyntextFFI"]),
        .testTarget(
            name: "SyntextTests",
            dependencies: ["Syntext"]),
        .testTarget(
            name: "SyntextContextTests",
            dependencies: ["SyntextContext"]),
    ]
)
