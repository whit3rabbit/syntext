//! Unit tests for git checkout identity.
//!
//! Fixtures are hand-built `.git` directories and pointer files rather than
//! real `git` invocations: the production code never shells out, so neither
//! should the tests that pin its parsing.

use std::fs;
use std::path::Path;

use super::{classify_nested, identify_checkout, is_checkout_root, CheckoutKind};

/// Write `<root>/.git/HEAD` with `head` and return `root`.
fn plain_checkout(root: &Path, head: &str) {
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::write(root.join(".git").join("HEAD"), head).unwrap();
}

/// Write a `.git` pointer file at `root` aiming at `gitdir`, and give that
/// gitdir a HEAD.
fn pointer_checkout(root: &Path, gitdir: &Path, pointer_text: &str, head: &str) {
    fs::create_dir_all(root).unwrap();
    fs::create_dir_all(gitdir).unwrap();
    fs::write(gitdir.join("HEAD"), head).unwrap();
    fs::write(root.join(".git"), pointer_text).unwrap();
}

#[test]
fn plain_checkout_is_main_with_branch() {
    let tmp = tempfile::tempdir().unwrap();
    plain_checkout(tmp.path(), "ref: refs/heads/main\n");

    let id = identify_checkout(tmp.path()).expect("a .git directory is a checkout");
    assert_eq!(id.kind, CheckoutKind::Main);
    assert_eq!(id.name, None);
    assert_eq!(id.branch.as_deref(), Some("main"));
    assert_eq!(id.head, None);
}

#[test]
fn pointer_into_worktrees_is_a_linked_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    let gitdir = tmp
        .path()
        .join("main")
        .join(".git")
        .join("worktrees")
        .join("foo");
    let root = tmp.path().join("wt-foo");
    let pointer = format!("gitdir: {}\n", gitdir.display());
    pointer_checkout(&root, &gitdir, &pointer, "ref: refs/heads/feature/x\n");

    let id = identify_checkout(&root).expect("a .git pointer file is a checkout");
    assert_eq!(id.kind, CheckoutKind::Linked);
    assert_eq!(id.name.as_deref(), Some("foo"));
    assert_eq!(id.branch.as_deref(), Some("feature/x"));
}

#[test]
fn relative_pointer_into_modules_is_a_submodule() {
    let tmp = tempfile::tempdir().unwrap();
    // Layout: <tmp>/super/.git/modules/vendor/lib, worktree at <tmp>/super/vendor/lib.
    let gitdir = tmp
        .path()
        .join("super")
        .join(".git")
        .join("modules")
        .join("vendor")
        .join("lib");
    let root = tmp.path().join("super").join("vendor").join("lib");
    // git writes this form: relative to the submodule's own worktree.
    pointer_checkout(
        &root,
        &gitdir,
        "gitdir: ../../.git/modules/vendor/lib\n",
        "ref: refs/heads/main\n",
    );

    let id = identify_checkout(&root).expect("relative gitdir must resolve");
    assert_eq!(id.kind, CheckoutKind::Submodule);
    assert_eq!(
        id.name.as_deref(),
        Some("vendor/lib"),
        "nested submodule names keep their path shape, forward-slashed"
    );
}

#[test]
fn detached_head_reports_a_commit_not_a_branch() {
    let tmp = tempfile::tempdir().unwrap();
    let sha = "0123456789abcdef0123456789abcdef01234567";
    plain_checkout(tmp.path(), &format!("{sha}\n"));

    let id = identify_checkout(tmp.path()).unwrap();
    assert_eq!(id.branch, None);
    assert_eq!(id.head.as_deref(), Some(sha));
}

#[test]
fn rightmost_marker_wins() {
    // A repo that merely lives under a directory called `worktrees` must not be
    // read as a linked worktree of it.
    let tmp = tempfile::tempdir().unwrap();
    let gitdir = tmp
        .path()
        .join("worktrees")
        .join("proj")
        .join(".git")
        .join("modules")
        .join("sub");
    let root = tmp.path().join("checkout");
    let pointer = format!("gitdir: {}\n", gitdir.display());
    pointer_checkout(&root, &gitdir, &pointer, "ref: refs/heads/main\n");

    let id = identify_checkout(&root).unwrap();
    assert_eq!(id.kind, CheckoutKind::Submodule);
    assert_eq!(id.name.as_deref(), Some("sub"));
}

#[test]
fn control_characters_in_head_are_rejected_not_scrubbed() {
    let tmp = tempfile::tempdir().unwrap();
    // A ref name carrying an ANSI escape: git forbids this, so treat the file
    // as hostile and surface nothing rather than printing it to a terminal.
    plain_checkout(tmp.path(), "ref: refs/heads/ma\x1b[31min\n");

    let id = identify_checkout(tmp.path()).unwrap();
    assert_eq!(id.kind, CheckoutKind::Main);
    assert_eq!(
        id.branch, None,
        "escape sequence must not reach the terminal"
    );
}

#[test]
fn oversized_branch_label_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let long = "b".repeat(300);
    plain_checkout(tmp.path(), &format!("ref: refs/heads/{long}\n"));

    assert_eq!(identify_checkout(tmp.path()).unwrap().branch, None);
}

#[test]
fn malformed_head_yields_neither_branch_nor_commit() {
    let tmp = tempfile::tempdir().unwrap();
    plain_checkout(tmp.path(), "not a ref and not a sha\n");

    let id = identify_checkout(tmp.path()).unwrap();
    assert_eq!(id.branch, None);
    assert_eq!(id.head, None);
}

#[test]
fn missing_head_still_identifies_the_kind() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir_all(tmp.path().join(".git")).unwrap();

    let id = identify_checkout(tmp.path()).expect("no HEAD is still a checkout");
    assert_eq!(id.kind, CheckoutKind::Main);
    assert_eq!(id.branch, None);
}

#[test]
fn a_directory_without_dot_git_is_not_a_checkout() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir_all(tmp.path().join("src")).unwrap();

    assert!(!is_checkout_root(&tmp.path().join("src")));
    assert!(identify_checkout(&tmp.path().join("src")).is_none());
}

#[test]
fn is_checkout_root_accepts_both_pointer_file_and_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let as_dir = tmp.path().join("clone");
    plain_checkout(&as_dir, "ref: refs/heads/main\n");
    let as_file = tmp.path().join("wt");
    fs::create_dir_all(&as_file).unwrap();
    fs::write(as_file.join(".git"), "gitdir: /nowhere\n").unwrap();

    assert!(is_checkout_root(&as_dir));
    assert!(is_checkout_root(&as_file));
}

#[test]
fn nested_git_directory_classifies_as_a_nested_clone() {
    let tmp = tempfile::tempdir().unwrap();
    let nested = tmp.path().join("vendor").join("thing");
    plain_checkout(&nested, "ref: refs/heads/main\n");

    // `Main` describes the root being indexed, never a subtree of it.
    assert_eq!(classify_nested(&nested), CheckoutKind::NestedClone);
}

#[test]
fn unparseable_pointer_classifies_as_a_nested_clone() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("odd");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join(".git"), "this is not a gitdir line\n").unwrap();

    assert!(
        is_checkout_root(&root),
        "must still fail closed for the walk"
    );
    assert_eq!(classify_nested(&root), CheckoutKind::NestedClone);
    assert!(identify_checkout(&root).is_none());
}

#[test]
fn classify_nested_names_worktrees_and_submodules() {
    let tmp = tempfile::tempdir().unwrap();
    let wt_gitdir = tmp.path().join(".git").join("worktrees").join("a");
    let wt = tmp.path().join("wt-a");
    pointer_checkout(
        &wt,
        &wt_gitdir,
        &format!("gitdir: {}\n", wt_gitdir.display()),
        "ref: refs/heads/a\n",
    );
    let sm_gitdir = tmp.path().join(".git").join("modules").join("b");
    let sm = tmp.path().join("sub-b");
    pointer_checkout(
        &sm,
        &sm_gitdir,
        &format!("gitdir: {}\n", sm_gitdir.display()),
        "ref: refs/heads/b\n",
    );

    assert_eq!(classify_nested(&wt), CheckoutKind::Linked);
    assert_eq!(classify_nested(&sm), CheckoutKind::Submodule);
    assert_eq!(CheckoutKind::Linked.noun(), "worktree");
    assert_eq!(CheckoutKind::Submodule.noun(), "submodule");
    assert_eq!(CheckoutKind::NestedClone.noun(), "checkout");
}
