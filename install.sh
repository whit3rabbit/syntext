#!/usr/bin/env sh
# install.sh -- download and install syntext (st)
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/whit3rabbit/syntext/main/install.sh | sh
#
# Environment:
#   SYNTEXT_VERSION  -- version to install (default: 1.0.0)
#   INSTALL_DIR      -- directory to install st binary (default: /usr/local/bin)

set -eu

SYNTEXT_VERSION="${SYNTEXT_VERSION:-1.0.0}"
INSTALL_DIR="${INSTALL_DIR:-/usr/local/bin}"
BASE_URL="https://github.com/whit3rabbit/syntext/releases/download/v${SYNTEXT_VERSION}"

die() {
    echo "error: $*" >&2
    exit 1
}

need_cmd() {
    command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"
}

verify_sha256() {
    # $1 = file, $2 = expected hex digest
    file="$1"
    expected="$2"
    if command -v sha256sum >/dev/null 2>&1; then
        actual=$(sha256sum "$file" | awk '{print $1}')
    elif command -v shasum >/dev/null 2>&1; then
        actual=$(shasum -a 256 "$file" | awk '{print $1}')
    else
        die "neither sha256sum nor shasum found; cannot verify checksum"
    fi
    if [ "$actual" != "$expected" ]; then
        die "checksum mismatch for $file\n  expected: $expected\n  got:      $actual"
    fi
}

fetch_checksum() {
    # Extract the SHA256 for a given filename from the SHA256SUMS file
    filename="$1"
    sums_file="$2"
    awk -v f="$filename" '$2 == f || $2 == "./"f {print $1; exit}' "$sums_file"
}

OS=$(uname -s)
ARCH=$(uname -m)

# ── macOS ────────────────────────────────────────────────────────────────────

if [ "$OS" = "Darwin" ]; then
    if command -v brew >/dev/null 2>&1; then
        echo "==> Homebrew detected. Installing syntext via cask..."
        brew tap whit3rabbit/tap
        brew install --cask whit3rabbit/tap/syntext
        echo "==> Installed syntext via Homebrew cask."
        echo "    To upgrade later: brew upgrade --cask whit3rabbit/tap/syntext"
        exit 0
    fi

    # No brew -- fall back to zip download
    echo "==> Homebrew not found. Falling back to direct download..."
    need_cmd curl
    need_cmd unzip

    case "$ARCH" in
        arm64|aarch64) ARTIFACT="st-${SYNTEXT_VERSION}-macos-arm64.zip" ;;
        x86_64)        ARTIFACT="st-${SYNTEXT_VERSION}-macos-x86_64.zip" ;;
        *) die "unsupported macOS architecture: $ARCH" ;;
    esac

    TMPDIR=$(mktemp -d)
    trap 'rm -rf "$TMPDIR"' EXIT

    echo "==> Downloading ${ARTIFACT}..."
    curl -fsSL "${BASE_URL}/${ARTIFACT}" -o "${TMPDIR}/${ARTIFACT}"

    echo "==> Verifying checksum..."
    curl -fsSL "${BASE_URL}/SHA256SUMS" -o "${TMPDIR}/SHA256SUMS"
    EXPECTED=$(fetch_checksum "$ARTIFACT" "${TMPDIR}/SHA256SUMS")
    [ -n "$EXPECTED" ] || die "no checksum entry found for $ARTIFACT in SHA256SUMS"
    verify_sha256 "${TMPDIR}/${ARTIFACT}" "$EXPECTED"

    unzip -q "${TMPDIR}/${ARTIFACT}" -d "${TMPDIR}/unzip"
    [ -f "${TMPDIR}/unzip/st" ] || die "st binary not found inside $ARTIFACT"
    chmod +x "${TMPDIR}/unzip/st"

    if [ -w "$INSTALL_DIR" ]; then
        mv "${TMPDIR}/unzip/st" "${INSTALL_DIR}/st"
    else
        sudo mv "${TMPDIR}/unzip/st" "${INSTALL_DIR}/st"
    fi

    echo "==> Installed st to ${INSTALL_DIR}/st"
    "${INSTALL_DIR}/st" --version
    exit 0
fi

# ── Linux ────────────────────────────────────────────────────────────────────

if [ "$OS" = "Linux" ]; then
    need_cmd curl

    TMPDIR=$(mktemp -d)
    trap 'rm -rf "$TMPDIR"' EXIT

    echo "==> Downloading SHA256SUMS..."
    curl -fsSL "${BASE_URL}/SHA256SUMS" -o "${TMPDIR}/SHA256SUMS"

    # Prefer deb on amd64 Debian/Ubuntu systems
    if [ "$ARCH" = "x86_64" ] && command -v dpkg >/dev/null 2>&1; then
        DEB="syntext_${SYNTEXT_VERSION}_amd64.deb"
        echo "==> Downloading ${DEB}..."
        curl -fsSL "${BASE_URL}/${DEB}" -o "${TMPDIR}/${DEB}"

        echo "==> Verifying checksum..."
        EXPECTED=$(fetch_checksum "$DEB" "${TMPDIR}/SHA256SUMS")
        [ -n "$EXPECTED" ] || die "no checksum entry found for $DEB in SHA256SUMS"
        verify_sha256 "${TMPDIR}/${DEB}" "$EXPECTED"

        echo "==> Installing via dpkg..."
        sudo dpkg -i "${TMPDIR}/${DEB}"
        echo "==> Installed syntext via .deb package."
        st --version
        exit 0
    fi

    # Raw binary fallback
    case "$ARCH" in
        x86_64)        ARTIFACT="st-${SYNTEXT_VERSION}-linux-amd64" ;;
        aarch64|arm64) ARTIFACT="st-${SYNTEXT_VERSION}-linux-arm64" ;;
        *) die "unsupported Linux architecture: $ARCH" ;;
    esac

    echo "==> Downloading ${ARTIFACT}..."
    curl -fsSL "${BASE_URL}/${ARTIFACT}" -o "${TMPDIR}/st"

    echo "==> Verifying checksum..."
    EXPECTED=$(fetch_checksum "$ARTIFACT" "${TMPDIR}/SHA256SUMS")
    [ -n "$EXPECTED" ] || die "no checksum entry found for $ARTIFACT in SHA256SUMS"
    verify_sha256 "${TMPDIR}/st" "$EXPECTED"

    chmod +x "${TMPDIR}/st"
    if [ -w "$INSTALL_DIR" ]; then
        mv "${TMPDIR}/st" "${INSTALL_DIR}/st"
    else
        sudo mv "${TMPDIR}/st" "${INSTALL_DIR}/st"
    fi

    echo "==> Installed st to ${INSTALL_DIR}/st"
    "${INSTALL_DIR}/st" --version
    exit 0
fi

die "unsupported OS: $OS (supported: Darwin, Linux)"
