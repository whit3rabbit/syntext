#!/usr/bin/env sh
# install.sh -- download and install syntext (st)
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/whit3rabbit/syntext/main/install.sh | sh
#
# Environment:
#   SYNTEXT_VERSION  -- version to install (default: 2.4.0)
#   INSTALL_DIR      -- directory to install st binary (default: /usr/local/bin)

set -eu

SYNTEXT_VERSION="${SYNTEXT_VERSION:-2.4.0}"
INSTALL_DIR="${INSTALL_DIR:-/usr/local/bin}"
BASE_URL="https://github.com/whit3rabbit/syntext/releases/download/v${SYNTEXT_VERSION}"

# Minimum glibc version required by the dynamically linked (gnu) Linux
# binaries. Older systems, musl-based systems, and systems we cannot probe
# install the fully static *-musl binaries instead. Measured 2026-09-30 from
# the release binaries built by cross's current (Ubuntu 24.04-based) images:
# the gnu binaries reference GLIBC_2.39 symbols. Refresh from the "Report
# glibc floor" step of the release workflow's build-linux job.
REQUIRED_GLIBC="2.39"

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

detect_libc() {
    # Print "musl", "glibc/<version>", or "unknown". Anything ambiguous must
    # resolve toward musl (the static binary runs everywhere).
    if ! command -v ldd >/dev/null 2>&1; then
        echo "unknown"
        return 0
    fi
    first_line=$(ldd --version 2>&1 | head -n 1)
    case "$first_line" in
        *musl*) echo "musl"; return 0 ;;
    esac
    ver=$(printf '%s\n' "$first_line" | grep -oE '[0-9]+\.[0-9]+(\.[0-9]+)?' | tail -n 1)
    if [ -n "$ver" ]; then
        echo "glibc/${ver}"
    else
        echo "unknown"
    fi
}

glibc_ge() {
    # Succeed if glibc version $1 >= required version $2 (numeric, component-wise).
    awk -v a="$1" -v b="$2" 'BEGIN {
        na = split(a, va, "."); nb = split(b, vb, ".");
        m = na > nb ? na : nb;
        for (i = 1; i <= m; i++) {
            x = (i <= na) ? va[i] + 0 : 0;
            y = (i <= nb) ? vb[i] + 0 : 0;
            if (x != y) exit (x < y);
        }
    }'
}

install_linux_binary() {
    # Download, checksum-verify, and install a raw prebuilt st binary.
    # $1 = artifact filename on the release. Requires TMPDIR populated with
    # SHA256SUMS (exits on success).
    artifact="$1"
    echo "==> Downloading ${artifact}..."
    curl -fsSL "${BASE_URL}/${artifact}" -o "${TMPDIR}/st"

    echo "==> Verifying checksum..."
    EXPECTED=$(fetch_checksum "$artifact" "${TMPDIR}/SHA256SUMS")
    [ -n "$EXPECTED" ] || die "no checksum entry found for $artifact in SHA256SUMS"
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

    LIBC=$(detect_libc)

    # Fully static musl binaries run on any Linux; use them whenever the gnu
    # binaries might not (musl system, unprobeable libc, or glibc too old --
    # in which case the .deb would install a binary that dies with
    # "GLIBC_x.yy not found" at runtime, so the deb path is skipped too).
    USE_MUSL=0
    case "$LIBC" in
        musl|unknown)
            USE_MUSL=1
            echo "==> libc: ${LIBC}; using static musl build."
            ;;
        glibc/*)
            GLIBC_VER=${LIBC#glibc/}
            if glibc_ge "$GLIBC_VER" "$REQUIRED_GLIBC"; then
                echo "==> glibc ${GLIBC_VER} >= ${REQUIRED_GLIBC}; using gnu build."
            else
                USE_MUSL=1
                echo "==> glibc ${GLIBC_VER} < ${REQUIRED_GLIBC}; using static musl build."
            fi
            ;;
    esac

    TMPDIR=$(mktemp -d)
    trap 'rm -rf "$TMPDIR"' EXIT

    echo "==> Downloading SHA256SUMS..."
    curl -fsSL "${BASE_URL}/SHA256SUMS" -o "${TMPDIR}/SHA256SUMS"

    if [ "$USE_MUSL" = "1" ]; then
        case "$ARCH" in
            x86_64)        ARTIFACT="st-${SYNTEXT_VERSION}-linux-amd64-musl" ;;
            aarch64|arm64) ARTIFACT="st-${SYNTEXT_VERSION}-linux-arm64-musl" ;;
            *) die "unsupported Linux architecture: $ARCH" ;;
        esac
        install_linux_binary "$ARTIFACT"
    fi

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

    # Raw gnu binary fallback
    case "$ARCH" in
        x86_64)        ARTIFACT="st-${SYNTEXT_VERSION}-linux-amd64" ;;
        aarch64|arm64) ARTIFACT="st-${SYNTEXT_VERSION}-linux-arm64" ;;
        *) die "unsupported Linux architecture: $ARCH" ;;
    esac
    install_linux_binary "$ARTIFACT"
fi

die "unsupported OS: $OS (supported: Darwin, Linux)"
