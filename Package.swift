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
        url: "https://github.com/whit3rabbit/syntext/releases/download/v2.4.0/syntext-swift-2.4.0.xcframework.zip",
        checksum: "f592047a9f7b42db165d4d5a00369971c304fe50b83e9266ee99719d40ba3b3b")

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
            path: "swift/Sources/CSyntext",
            publicHeadersPath: "include"),
        .target(
            name: "Syntext",
            dependencies: ["CSyntext", "SyntextFFI"],
            path: "swift/Sources/Syntext"),
        .testTarget(
            name: "SyntextTests",
            dependencies: ["Syntext"],
            path: "swift/Tests/SyntextTests"),
    ]
)
