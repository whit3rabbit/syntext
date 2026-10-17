// swift-tools-version:5.9
import PackageDescription
import Foundation

// Binary-target resolution, in priority order:
//
// 1. Dev/CI: swift/Scripts/build-xcframework.sh drops a local xcframework at
//    swift/build/SyntextFFI.xcframework; when present it is preferred.
// 2. Consumers: the pinned release zip for remote SPM / Xcode dependencies.
let hasLocalFFI = FileManager.default.fileExists(atPath: "swift/build/SyntextFFI.xcframework")
let ffiTarget: Target = hasLocalFFI
    ? .binaryTarget(name: "SyntextFFI", path: "swift/build/SyntextFFI.xcframework")
    : .binaryTarget(
        name: "SyntextFFI",
        url: "https://github.com/whit3rabbit/syntext/releases/download/v2.7.1/syntext-swift-2.7.1.xcframework.zip",
        checksum: "b1b2faf4ee3cb42f118c2172114c78ed846b94ac1eedf668d93e54f9cd33c477")

let package = Package(
    name: "Syntext",
    platforms: [.macOS(.v12)],
    products: [
        .library(name: "Syntext", targets: ["Syntext"]),
        // The context engine (docs/CONTEXT.md); see swift/Package.swift.
        .library(name: "SyntextContext", targets: ["SyntextContext"]),
    ],
    targets: [
        ffiTarget,
        .target(
            name: "CSyntext",
            dependencies: ["SyntextFFI"],
            path: "swift/Sources/CSyntext",
            publicHeadersPath: "include"),
        .target(
            name: "Syntext",
            dependencies: ["CSyntext", "SyntextFFI"],
            path: "swift/Sources/Syntext"),
        .target(
            name: "SyntextContext",
            dependencies: ["Syntext", "CSyntext", "SyntextFFI"],
            path: "swift/Sources/SyntextContext"),
        .testTarget(
            name: "SyntextTests",
            dependencies: ["Syntext"],
            path: "swift/Tests/SyntextTests"),
        .testTarget(
            name: "SyntextContextTests",
            dependencies: ["SyntextContext"],
            path: "swift/Tests/SyntextContextTests"),
    ]
)
