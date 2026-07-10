# Release Process and Checklist

This document details the step-by-step procedure for releasing a new version of `syntext` (`st`). Releases are initiated by pushing a Git tag (matching `v*`), which triggers the automated release workflows to build cross-platform binaries, publish to crates.io, update the Homebrew cask, and publish a GitHub Release containing automated release notes extracted from `CHANGELOG.md`.

---

## 1. Pre-Release Checklist (Preparation)

Perform these steps on your local machine before committing or tagging:

- [ ] **Bump Crate Version**: Update the `version` field in [Cargo.toml](../Cargo.toml).
- [ ] **Regenerate Lockfile**: Run `cargo check` or `cargo build` to ensure [Cargo.lock](../Cargo.lock) is updated with the new version.
- [ ] **Update Changelog**:
  - Open [CHANGELOG.md](../CHANGELOG.md).
  - Rename the `## [Unreleased]` section to `## [X.Y.Z] - YYYY-MM-DD` (matching your new version and current date).
  - Add a new, blank `## [Unreleased]` template section above the newly created version section.
- [ ] **Run Quality Gate**:
  - Run `cargo test --all-features` to ensure all unit and integration tests pass cleanly.
  - Run `cargo clippy --all-targets --all-features -- -D warnings` to ensure zero warnings.
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
  ```
- [ ] **Create Git Tag**: Create an annotated/signed tag for the new version:
  ```sh
  git tag -a vX.Y.Z -m "Release vX.Y.Z"
  ```
- [ ] **Push to Upstream**: Push the branch and the new tag to GitHub:
  ```sh
  git push origin main
  git push origin vX.Y.Z
  ```

---

## 3. Post-Release Checklist (Verification and Installer Bump)

> [!IMPORTANT]
> The automated installer defaults must only be updated **after** the release artifacts are successfully built and published. Pointing installers to a version tag before GHA publishes the assets will break installation attempts.

- [ ] **Monitor GitHub Actions**: Go to GitHub Actions and monitor the **Release** workflow. It will:
  - Build binaries for Linux (amd64, arm64), macOS (arm64, x86_64), Windows (amd64), and WASM (bundler).
  - Package and generate SHA256 checksums.
  - Create/publish a GitHub Release, using `scripts/extract_release_notes.py` to auto-populate the release description from `CHANGELOG.md`.
  - Publish the crate to [crates.io](https://crates.io/crates/syntext).
  - Automatically update the Homebrew cask in `whit3rabbit/homebrew-tap`.
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

---

## Release Automation Machinery

- **Changelog Extraction**: During the `release` job, `.github/workflows/release.yml` executes `python3 scripts/extract_release_notes.py` passing the version tag (e.g. `1.4.0`). This parses `CHANGELOG.md` and generates a temporary markdown file passed to `softprops/action-gh-release@v3` via `body_path`.
- **Cask Generation**: Homebrew cask updater runs `scripts/render_homebrew_cask.sh` to generate the Ruby formula with calculated SHA256 checksums from the newly built macOS archives, committing directly to the tap repository.
