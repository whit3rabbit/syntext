// swift-tools-version:5.9
import PackageDescription
import Foundation

// Binary-target resolution, in priority order:
//
// 1. Dev/CI: Scripts/build-xcframework.sh drops a local xcframework at
//    build/SyntextFFI.xcframework; when present it is preferred, so
//    `swift test` never needs an unreleased artifact.
// 2. Consumers: the pinned release zip. The `update-swift-package` release
//    job rewrites url+checksum after each release, so `main` always pins the
//    latest published xcframework. Caveat: an exact vX.Y.Z tag pins the
//    PREVIOUS release (the zip for X.Y.Z does not exist when that tag is
//    cut); pin `main` or a `swift-vX.Y.Z` tag instead — see docs/SWIFT.md.
let hasLocalFFI = FileManager.default.fileExists(atPath: "build/SyntextFFI.xcframework")
let ffiTarget: Target = hasLocalFFI
    ? .binaryTarget(name: "SyntextFFI", path: "build/SyntextFFI.xcframework")
    : .binaryTarget(
        name: "SyntextFFI",
        // Placeholder until the first release carrying the `ffi` feature
        // (v2.2.0); the pin job replaces it.
        url: "https://github.com/whit3rabbit/syntext/releases/download/v2.1.0/syntext-swift-2.1.0.xcframework.zip",
        checksum: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")

let package = Package(
    name: "Syntext",
    platforms: [.macOS(.v12)],
    products: [
        .library(name: "Syntext", targets: ["Syntext"]),
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
        .testTarget(
            name: "SyntextTests",
            dependencies: ["Syntext"]),
    ]
)
