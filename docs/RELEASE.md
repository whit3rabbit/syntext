# Release Process and Checklist

This document details the step-by-step procedure for releasing a new version of `syntext` (`st`). Releases are initiated by pushing a Git tag (matching `v*`), which triggers the automated release workflows to build cross-platform binaries, publish to crates.io, update the Homebrew cask, and publish a GitHub Release containing automated release notes extracted from `CHANGELOG.md`.

---

## 1. Pre-Release Checklist (Preparation)

Perform these steps on your local machine before committing or tagging:

- [ ] **Bump Crate Version**: Update the `version` field in [Cargo.toml](../Cargo.toml). [context/Cargo.toml](../context/Cargo.toml) requires `syntext` by path with a caret version (`version = "2.5.0"`), so a major version bump must update that requirement too.
- [ ] **Regenerate Lockfile**: Run `cargo check` or `cargo build` to ensure [Cargo.lock](../Cargo.lock) is updated with the new version.
- [ ] **Update Changelog**:
  - Open [CHANGELOG.md](../CHANGELOG.md).
  - Rename the `## [Unreleased]` section to `## [X.Y.Z] - YYYY-MM-DD` (matching your new version and current date).
  - Add a new, blank `## [Unreleased]` template section above the newly created version section.
- [ ] **Run Quality Gate**:
  - Run `cargo test --features cli,symbols,ffi,oracle` to ensure all unit and integration tests pass cleanly. (`wasm` is checked separately below; it is mutually exclusive with `ffi`/`native`, so a literal `--all-features` never compiles.)
  - Run `cargo clippy --all-targets --features cli,symbols,ffi,oracle -- -D warnings` to ensure zero warnings.
  - Run `cargo test -p syntext-context --features ffi` and `cargo clippy -p syntext-context --all-targets --features ffi --no-deps -- -D warnings`. The commands above build only the root `syntext` package, not the context engine in `context/`.
  - Run `cargo check --target wasm32-unknown-unknown --features wasm --no-default-features` to verify the wasm build still type-checks.
  - Run `cargo fmt --check` to ensure formatting is correct.
  - Ensure no source code file (excluding test suites) exceeds 400 lines (check `AGENTS.md` rules for module splitting guidelines).
  - Run the differential oracle test suite if applicable: `cargo test --features oracle`.
- [ ] **Verify Benchmark Latency**:
  - Run Criterion benchmarks to ensure no performance regression:
    ```sh
    cargo bench --bench query_latency -- --sample-size 10
    cargo bench --bench freshness -- --sample-size 10
    ```

---

## 2. Release Checklist (Tagging and Pushing)

Once preparation is complete and verified:

- [ ] **Commit Changes**: Commit the modified `Cargo.toml`, `Cargo.lock`, and `CHANGELOG.md` files:
  ```sh
  git add Cargo.toml Cargo.lock CHANGELOG.md
  git commit -m "release: vX.Y.Z"
  git push origin main
  ```
- [ ] **Stage the Swift xcframework**: Run the **Swift prepare** workflow on `main` (Actions > Swift prepare > Run workflow, or `gh workflow run swift-prepare.yml --ref main`). It builds `syntext-swift-X.Y.Z.xcframework.zip` once, stages it on a draft release `vX.Y.Z`, and opens a `swift-pin/vX.Y.Z` PR that pins both `Package.swift` files to it. Merge that PR. This is what makes the `vX.Y.Z` tag pin its own zip, so SwiftPM consumers can use `exact: "X.Y.Z"`. Tag right after merging: until the release is published, `main` pins a URL that is not downloadable yet.
- [ ] **Create Git Tag**: Tag the pin PR's merge commit, not the release commit. Create an annotated/signed tag for the new version:
  ```sh
  git pull origin main   # brings in the pin PR's merge commit
  git tag -a vX.Y.Z -m "Release vX.Y.Z"
  ```
- [ ] **Push the Tag**: The branch is already on GitHub; push the new tag:
  ```sh
  git push origin vX.Y.Z
  ```

---

## 3. Post-Release Checklist (Verification and Installer Bump)

> [!IMPORTANT]
> The automated installer defaults must only be updated **after** the release artifacts are successfully built and published. Pointing installers to a version tag before GHA publishes the assets will break installation attempts.

- [ ] **Monitor GitHub Actions**: Go to GitHub Actions and monitor the **Release** workflow. It will:
  - Build binaries for Linux (amd64, arm64, plus fully static musl variants of both), macOS (arm64, x86_64), Windows (amd64), and WASM (bundler).
  - Build the `SyntextFFI.xcframework` zip (Swift package binary target) from `syntext-context` with its `ffi` feature, so one library exports both `syntext.h` and `syntext_context.h` (the `Syntext` and `SyntextContext` products).
  - Package and generate SHA256 checksums.
  - Create/publish a GitHub Release, using `scripts/extract_release_notes.py` to auto-populate the release description from `CHANGELOG.md`.
  - Publish the `syntext` crate to [crates.io](https://crates.io/crates/syntext). `syntext-context` is `publish = false`, and the published package leaves out `context/`.
  - Automatically update the Homebrew cask in `whit3rabbit/homebrew-tap`.
  - Swift, staged path (the tag pins its own zip): the `swift-staged` job downloads the zip from the draft release and fails the release if its checksum differs from the tag's pin. The release job publishes that draft with the other artifacts, without rebuilding the zip, and tags `swift-vX.Y.Z` at the same commit for consumers of the old convention.
  - Swift, legacy path (the prepare workflow was skipped): the run warns, `build-swift` builds a zip, and `update-swift-package` pins `main` afterwards and tags `swift-vX.Y.Z`. That `vX.Y.Z` tag then pins the previous release's zip, the defect the prepare workflow exists to avoid. If the pin push is blocked (branch protection on `main` rejecting the workflow token), pin manually with `scripts/pin_swift_package.py X.Y.Z <sha256>`.
  - Verify the musl entries passed the `Verify static linking` step, and that the x86_64 musl `Smoke test` step ran the binary inside the `manylinux2014` (glibc 2.17) container.
  - Check the **Linux glibc floor** job summary (from the gnu amd64 build's `Report glibc floor` step): if the printed floor exceeds `REQUIRED_GLIBC` in `install.sh`, bump the constant in the same commit as the installer-defaults bump below. Note: releases before the musl artifacts existed (≤ 2.5.0) have no `*-musl` assets, so pinning `SYNTEXT_VERSION` that far back on an old-glibc box still fails at download.
- [ ] **Bump Installer Defaults**: Once the GitHub Release has successfully published:
  - Update `README.md`:
    - Status badge/version strings referencing the stable release.
    - Version constants in install examples.
  - Update `install.sh`:
    - `SYNTEXT_VERSION` default value on line 13.
    - Default version comment on line 8.
  - Commit these updates directly to `main`:
    ```sh
    git commit -am "chore: bump installer defaults to X.Y.Z post-release"
    git push origin main
    ```
- [ ] **Verify Installer**: Run a quick validation to ensure the installer works:
  ```sh
  SYNTEXT_VERSION=X.Y.Z curl -fsSL https://raw.githubusercontent.com/whit3rabbit/syntext/main/install.sh | sh
  ```
  Also spot-check the musl fallback on systems the gnu binaries cannot serve (both need Docker):
  ```sh
  # Alpine (musl-native) and an old-glibc distro must both pick the *-musl artifact:
  docker run --rm -i alpine sh -c "curl -fsSL https://raw.githubusercontent.com/whit3rabbit/syntext/main/install.sh | SYNTEXT_VERSION=X.Y.Z sh"
  docker run --rm -i quay.io/pypa/manylinux2014_x86_64 sh -c "curl -fsSL https://raw.githubusercontent.com/whit3rabbit/syntext/main/install.sh | SYNTEXT_VERSION=X.Y.Z sh"
  ```

---

## Release Automation Machinery

- **Changelog Extraction**: During the `release` job, `.github/workflows/release.yml` executes `python3 scripts/extract_release_notes.py` passing the version tag (e.g. `1.4.0`). This parses `CHANGELOG.md` and generates a temporary markdown file passed to `softprops/action-gh-release@v3` via `body_path`.
- **Cask Generation**: Homebrew cask updater runs `scripts/render_homebrew_cask.sh` to generate the Ruby formula with calculated SHA256 checksums from the newly built macOS archives, committing directly to the tap repository.
- **Swift xcframework**: `swift/Scripts/build-xcframework.sh` builds both Apple targets on one `macos-14` runner (static archives need no cross linker), with `MACOSX_DEPLOYMENT_TARGET` defaulting to the package's 12.0 floor. It fails if any archive member is built for a newer macOS, which is how 2.7.0 shipped blake3's C objects for 14.5. The Swift prepare workflow (`.github/workflows/swift-prepare.yml`) runs it before tagging and pins the zip with `scripts/pin_swift_package.py`. In the Release workflow, `swift-staged` reuses that zip, and the legacy `build-swift` and `update-swift-package` jobs run only for a tag without its own pin. A rebuilt zip never matches a recorded checksum, so the staged zip is published as is. See docs/SWIFT.md for the consumer-facing pin semantics.
