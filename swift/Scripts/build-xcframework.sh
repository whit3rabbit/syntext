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

# The package's platform floor (`.macOS(.v12)` in Package.swift). rustc and
# the cc crate both read MACOSX_DEPLOYMENT_TARGET. Without it, cc-built C
# objects (blake3's NEON/SSE kernels) take the build machine's SDK version:
# 2.7.0 shipped `blake3_neon.o` for macOS 14.5, and every app with a lower
# floor got an ld "built for newer macOS version" warning.
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-12.0}"

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

# Fail rather than ship a member built above the floor. `otool -l` prints a
# `minos` per object (LC_BUILD_VERSION) for every member of every slice.
echo "==> check minimum macOS of every archive member (<= $MACOSX_DEPLOYMENT_TARGET)"
MAX_MINOS="$(otool -l swift/build/lib/libsyntext.a | awk '$1 == "minos" { print $2 }' | sort -V | tail -1)"
if [[ -z "$MAX_MINOS" ]]; then
  echo "error: no LC_BUILD_VERSION found in swift/build/lib/libsyntext.a" >&2
  exit 1
fi
if [[ "$(printf '%s\n%s\n' "$MAX_MINOS" "$MACOSX_DEPLOYMENT_TARGET" | sort -V | tail -1)" != "$MACOSX_DEPLOYMENT_TARGET" ]]; then
  echo "error: an archive member requires macOS $MAX_MINOS, above the $MACOSX_DEPLOYMENT_TARGET floor" >&2
  otool -l swift/build/lib/libsyntext.a \
    | awk -v max="$MAX_MINOS" '/libsyntext\.a\(/ { member = $0 } $1 == "minos" && $2 == max { print member } ' >&2
  exit 1
fi
echo "    highest minos: $MAX_MINOS"

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
