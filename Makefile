# syntext Makefile
# Linux cross-compilation uses `cross` (Docker required).
# macOS targets must be built natively on a macOS host.
# Install build tools: make install-tools

VERSION  := $(shell grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)"/\1/')
BINARY   := st
DIST_DIR := dist

.PHONY: all build test lint clean dist install-tools \
        build-linux-amd64 build-linux-arm64 \
        build-macos-x86_64 build-macos-arm64 \
        deb-amd64 deb-arm64

all: build

# ── Local build ────────────────────────────────────────────────────────────────

build:
	cargo build --release

test:
	cargo test

lint:
	cargo clippy -- -D warnings

clean:
	cargo clean
	rm -rf $(DIST_DIR)

# ── Tool installation ──────────────────────────────────────────────────────────
# cross: Linux cross-compilation via Docker
# cargo-deb: .deb packaging

install-tools:
	cargo install cross --git https://github.com/cross-rs/cross
	cargo install cargo-deb

# ── Linux builds (cross, requires Docker) ─────────────────────────────────────

build-linux-amd64:
	cross build --release --target x86_64-unknown-linux-gnu
	mkdir -p $(DIST_DIR)
	cp target/x86_64-unknown-linux-gnu/release/$(BINARY) \
	   $(DIST_DIR)/$(BINARY)-$(VERSION)-linux-amd64

build-linux-arm64:
	cross build --release --target aarch64-unknown-linux-gnu
	mkdir -p $(DIST_DIR)
	cp target/aarch64-unknown-linux-gnu/release/$(BINARY) \
	   $(DIST_DIR)/$(BINARY)-$(VERSION)-linux-arm64

# ── macOS builds (native only, no cross-compilation to macOS) ─────────────────

build-macos-arm64:
	cargo build --release --target aarch64-apple-darwin
	mkdir -p $(DIST_DIR)
	cp target/aarch64-apple-darwin/release/$(BINARY) \
	   $(DIST_DIR)/$(BINARY)-$(VERSION)-macos-arm64

build-macos-x86_64:
	cargo build --release --target x86_64-apple-darwin
	mkdir -p $(DIST_DIR)
	cp target/x86_64-apple-darwin/release/$(BINARY) \
	   $(DIST_DIR)/$(BINARY)-$(VERSION)-macos-x86_64

# ── Debian packages ───────────────────────────────────────────────────────────
# --no-build: package the binary that cross already produced.
# Output: dist/syntext_<version>_<arch>.deb

deb-amd64: build-linux-amd64
	cargo deb --no-build --target x86_64-unknown-linux-gnu \
	  --output $(DIST_DIR)/syntext_$(VERSION)_amd64.deb

deb-arm64: build-linux-arm64
	cargo deb --no-build --target aarch64-unknown-linux-gnu \
	  --output $(DIST_DIR)/syntext_$(VERSION)_arm64.deb

# ── Full distribution artifacts ───────────────────────────────────────────────
# Builds all Linux targets + debs. Run macOS targets separately on a macOS host.

dist: deb-amd64 deb-arm64
	@echo "Artifacts in $(DIST_DIR)/:"
	@ls -lh $(DIST_DIR)/
