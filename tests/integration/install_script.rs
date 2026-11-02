//! `install.sh` on its Homebrew path, driven through `brew` and `uname` shims.
//!
//! A cask installs only its current version, so the script must not report
//! success for a version it did not install. Unix only: the script is POSIX sh.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

fn shim(dir: &Path, name: &str, body: &str) {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// Run the script as if on macOS, with a cask that installs `cask_version`.
fn run_install(cask_version: &str, pinned: Option<&str>) -> Output {
    let shims = tempfile::tempdir().unwrap();
    shim(
        shims.path(),
        "uname",
        r#"[ "$1" = "-s" ] && echo Darwin || echo arm64"#,
    );
    shim(
        shims.path(),
        "brew",
        &format!(r#"[ "$1" = "list" ] && echo "syntext {cask_version}"; exit 0"#),
    );

    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("install.sh");
    let path = format!(
        "{}:{}",
        shims.path().display(),
        std::env::var("PATH").unwrap()
    );
    let mut command = Command::new("sh");
    command
        .arg(script)
        .env("PATH", path)
        .env_remove("SYNTEXT_VERSION");
    if let Some(version) = pinned {
        command.env("SYNTEXT_VERSION", version);
    }
    command.output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn pin_matching_the_cask_succeeds() {
    let output = run_install("9.9.9", Some("9.9.9"));
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert!(text(&output.stdout).contains("Installed syntext 9.9.9"));
    assert!(text(&output.stderr).is_empty());
}

#[test]
fn pin_the_cask_cannot_honour_fails() {
    let output = run_install("1.0.0", Some("9.9.9"));
    assert!(!output.status.success());
    assert!(!text(&output.stdout).contains("Installed syntext"));
    let stderr = text(&output.stderr);
    assert!(stderr.contains("requested 9.9.9"), "{stderr}");
    assert!(stderr.contains("installed 1.0.0"), "{stderr}");
    assert!(stderr.contains("brew update"), "{stderr}");
}

#[test]
fn unpinned_mismatch_warns_and_names_the_real_version() {
    let output = run_install("1.0.0", None);
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert!(text(&output.stdout).contains("Installed syntext 1.0.0"));
    let stderr = text(&output.stderr);
    assert!(stderr.starts_with("warning:"), "{stderr}");
    assert!(stderr.contains("brew update"), "{stderr}");
}
