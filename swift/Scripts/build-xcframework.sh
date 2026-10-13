#!/usr/bin/env bash
# Build the SyntextFFI.xcframework (macOS arm64 + x86_64) from the Rust
# staticlib and stage a versioned, checksummed zip for release.
#
# The staticlib is built from the syntext-context crate with its `ffi`
# feature, which enables syntext's own: one library exports both C ABIs
# (syntext.h and syntext_context.h), because an app can link only one Rust
# static library.
#
# Usage:
#   ./swift/Scripts/build-xcframework.sh
#
# Outputs (all under swift/build/):
#   SyntextFFI.xcframework                     - used by Package.swift locally
#   syntext-swift-<version>.xcframework.zip    - release artifact
#   syntext-swift-<version>.xcframework.zip.sha256 - checksum (stdout too)
#
# If SYNTEXT_SWIFT_ZIP_OUT is set, the zip is also copied there (CI staging).
set -euo pipefail

cd "$(dirname "$0")/../.."

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
if [[ -z "$VERSION" ]]; then
  echo "error: could not read version from Cargo.toml" >&2
  exit 1
fi

TARGETS=(aarch64-apple-darwin x86_64-apple-darwin)
rustup target add "${TARGETS[@]}" >/dev/null

mkdir -p swift/build
rm -rf swift/build/SyntextFFI.xcframework

THIN=()
for T in "${TARGETS[@]}"; do
  echo "==> cargo rustc --target $T (staticlib, syntext + context ffi)"
  # Manifest crate types are unchanged; the staticlib is produced on demand
  # only, so plain builds/tests/publish are unaffected.
  cargo rustc -p syntext-context --lib --release --features ffi --target "$T" --crate-type staticlib
  THIN+=("target/$T/release/libsyntext_context.a")
done

# Static-library xcframework slices are per-PLATFORM, not per-arch: two .a
# slices for the same platform collide ("two equivalent library
# definitions"). Merge the thin archives into one universal library with
# lipo so macOS is a single slice covering both architectures.
echo "==> lipo (universal libsyntext.a)"
mkdir -p swift/build/lib
rm -f swift/build/lib/libsyntext.a
lipo -create "${THIN[@]}" -output swift/build/lib/libsyntext.a

echo "==> xcodebuild -create-xcframework"
xcodebuild -create-xcframework \
  -library swift/build/lib/libsyntext.a \
  -output swift/build/SyntextFFI.xcframework >/dev/null

ZIP="syntext-swift-${VERSION}.xcframework.zip"
echo "==> zip $ZIP"
(
  cd swift/build
  rm -f "$ZIP"
  zip -qr -X "$ZIP" SyntextFFI.xcframework
)

echo "==> checksum"
(
  cd swift
  swift package compute-checksum "build/$ZIP" | tee "build/$ZIP.sha256"
)

if [[ -n "${SYNTEXT_SWIFT_ZIP_OUT:-}" ]]; then
  mkdir -p "$SYNTEXT_SWIFT_ZIP_OUT"
  cp "swift/build/$ZIP" "swift/build/$ZIP.sha256" "$SYNTEXT_SWIFT_ZIP_OUT/"
fi

echo "==> done: swift/build/$ZIP (version $VERSION)"
