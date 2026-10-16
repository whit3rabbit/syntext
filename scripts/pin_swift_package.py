#!/usr/bin/env python3
"""Pin both Swift manifests to one release's xcframework zip.

Usage: scripts/pin_swift_package.py <version> <sha256>

Rewrites the binary target's `url:` and `checksum:` literals in
`Package.swift` (repository root, for consumers) and `swift/Package.swift`
(the package directory) to
`releases/download/v<version>/syntext-swift-<version>.xcframework.zip`.
Each file must contain exactly one of each literal, or nothing is written.
Used by the Swift prepare workflow (before tagging) and by the release
workflow's legacy pin job (after publishing).
"""

import pathlib
import re
import sys

URL_RE = re.compile(
    r'url: "https://github\.com/whit3rabbit/syntext/releases/download/[^"]*syntext-swift-[^"]*\.xcframework\.zip"'
)
SUM_RE = re.compile(r'checksum: "[0-9a-f]{64}"')
VERSION_RE = re.compile(r"^\d+\.\d+\.\d+$")
MANIFESTS = ("Package.swift", "swift/Package.swift")


def main(argv: list[str]) -> int:
    if len(argv) != 3:
        print(__doc__.strip().splitlines()[2], file=sys.stderr)
        return 2
    version, checksum = argv[1], argv[2].lower()
    if not VERSION_RE.match(version):
        print(f"error: version must be X.Y.Z, got {version!r}", file=sys.stderr)
        return 2
    if not re.fullmatch(r"[0-9a-f]{64}", checksum):
        print(f"error: checksum must be 64 hex characters, got {checksum!r}", file=sys.stderr)
        return 2

    url = (
        f'url: "https://github.com/whit3rabbit/syntext/releases/download/'
        f'v{version}/syntext-swift-{version}.xcframework.zip"'
    )
    patched = {}
    for name in MANIFESTS:
        path = pathlib.Path(name)
        text = path.read_text()
        text, n_url = URL_RE.subn(url, text)
        text, n_sum = SUM_RE.subn(f'checksum: "{checksum}"', text)
        if n_url != 1 or n_sum != 1:
            print(
                f"error: expected exactly one url and one checksum pin in {name}, "
                f"got {n_url}/{n_sum}",
                file=sys.stderr,
            )
            return 1
        patched[path] = text
    for path, text in patched.items():
        path.write_text(text)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
