use super::*;
use crate::error::ContextError;
use crate::store::{ContextStore, StoreOptions};

fn write(root: &Path, rel: &str, text: &[u8]) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn keys(store: &ContextStore) -> Vec<String> {
    store
        .snapshot()
        .documents()
        .into_iter()
        .map(|d| d.key)
        .collect()
}

fn store_in(dir: &Path) -> ContextStore {
    ContextStore::create(
        dir,
        StoreOptions {
            sync: false,
            ..StoreOptions::default()
        },
    )
    .unwrap()
}

#[test]
fn sync_adds_modifies_and_removes() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    write(root, ".git/config", b"[core]");
    write(root, "src/a.rs", b"fn alpha() {}\n");
    write(root, "docs/b.md", b"# Beta\n\nbeta docs\n");
    write(root, "logo.png", b"\x89PNG");
    write(root, "blob.dat", b"bin\0ary");
    let store_dir = tempfile::tempdir().unwrap();
    let store = store_in(store_dir.path());
    let source = DirectorySource::new("repo", root).unwrap();

    let r = store.sync_directory(&source).unwrap();
    assert_eq!((r.added, r.modified, r.removed), (2, 0, 0), "{r:?}");
    assert_eq!(r.skipped, 1, "NUL content skipped");
    assert_eq!(keys(&store), ["repo:docs/b.md", "repo:src/a.rs"]);

    let r = store.sync_directory(&source).unwrap();
    assert_eq!(
        (r.added, r.modified, r.removed),
        (0, 0, 0),
        "idempotent: {r:?}"
    );

    write(root, "src/a.rs", b"fn alpha() { changed() }\n");
    std::fs::remove_file(root.join("docs/b.md")).unwrap();
    let r = store.sync_directory(&source).unwrap();
    assert_eq!((r.added, r.modified, r.removed), (0, 1, 1), "{r:?}");
    let text = store
        .snapshot()
        .document_text("repo:src/a.rs")
        .unwrap()
        .unwrap();
    assert!(text.contains("changed"));
    assert_eq!(keys(&store), ["repo:src/a.rs"]);

    let doc = store.snapshot().document("repo:src/a.rs").unwrap();
    let on_disk = file_digest(&root.join("src/a.rs")).unwrap();
    assert_eq!(
        doc.source_hash,
        on_disk.to_hex(),
        "source hash matches syntext's digest"
    );
    drop(store);
}

#[test]
fn gitignored_excluded_and_store_paths_are_left_out() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    write(root, "keep.md", b"kept\n");
    write(root, "gen/out.md", b"generated\n");
    write(root, "vendor/lib.md", b"vendored\n");
    let store = store_in(&root.join(".ctx"));
    let source = DirectorySource::new("repo", root)
        .unwrap()
        .exclude("vendor");
    store.sync_directory(&source).unwrap();
    assert_eq!(keys(&store), ["repo:gen/out.md", "repo:keep.md"]);

    write(root, ".gitignore", b"gen/\n");
    let r = store.sync_directory(&source).unwrap();
    assert_eq!(r.removed, 1, "newly ignored file removed");
    let k = keys(&store);
    assert!(k.contains(&"repo:keep.md".to_string()));
    assert!(
        !k.iter().any(|k| k.contains("gen/") || k.contains(".ctx")),
        "{k:?}"
    );
    drop(store);
}

#[test]
fn read_only_handles_cannot_sync_and_labels_are_validated() {
    let dir = tempfile::tempdir().unwrap();
    drop(store_in(dir.path()));
    let ro = ContextStore::open_read_only(dir.path(), StoreOptions::default()).unwrap();
    let source = DirectorySource::new("repo", dir.path()).unwrap();
    assert!(matches!(
        ro.sync_directory(&source),
        Err(ContextError::ReadOnly)
    ));
    assert!(DirectorySource::new("Bad Label", dir.path()).is_err());
    drop(ro);
}

#[test]
fn binary_extension_list_is_sorted() {
    let list = binary_extensions();
    assert!(list.windows(2).all(|w| w[0] < w[1]));
}

#[test]
fn a_large_sync_is_batched_but_lands_in_one_generation() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    for i in 0..40 {
        write(
            root,
            &format!("src/m{i}.rs"),
            format!("/// Module {i}.\npub fn handler_{i}(x: u32) -> u32 {{\n    x + {i}\n}}\n")
                .as_bytes(),
        );
    }
    let store_dir = tempfile::tempdir().unwrap();
    let store = ContextStore::create(
        store_dir.path(),
        StoreOptions {
            sync: false,
            batch_bytes: 256,
            ..StoreOptions::default()
        },
    )
    .unwrap();
    let source = DirectorySource::new("repo", root).unwrap();
    let r = store.sync_directory(&source).unwrap();
    assert_eq!((r.added, r.generation), (40, 1), "{r:?}");
    let stats = store.stats();
    assert!(stats.segments > 4, "batched: {} segments", stats.segments);
    assert_eq!(stats.documents, 40);

    let r = store.sync_directory(&source).unwrap();
    assert_eq!((r.added, r.modified, r.generation), (0, 0, 1), "{r:?}");
    drop(store);
}

#[test]
fn a_file_that_turns_binary_is_removed() {
    let repo = tempfile::tempdir().unwrap();
    write(repo.path(), "data/table.csv", b"id,name\n1,alpha\n");
    let store_dir = tempfile::tempdir().unwrap();
    let store = store_in(store_dir.path());
    let source = DirectorySource::new("repo", repo.path()).unwrap();
    store.sync_directory(&source).unwrap();
    assert_eq!(keys(&store), ["repo:data/table.csv"]);

    write(repo.path(), "data/table.csv", b"\0\x01binary now");
    let r = store.sync_directory(&source).unwrap();
    assert_eq!((r.removed, r.skipped), (1, 1), "{r:?}");
    assert!(keys(&store).is_empty());
    drop(store);
}

#[test]
fn sync_paths_updates_only_the_named_files() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    write(root, "src/a.rs", b"fn alpha() {}\n");
    write(root, "src/b.rs", b"fn beta() {}\n");
    let store_dir = tempfile::tempdir().unwrap();
    let store = store_in(store_dir.path());
    let source = DirectorySource::new("repo", root).unwrap();
    store.sync_directory(&source).unwrap();

    write(root, "src/a.rs", b"fn alpha() { changed() }\n");
    write(root, "src/b.rs", b"fn beta() { also_changed() }\n");
    write(root, "src/c.rs", b"fn gamma() {}\n");
    // An absolute path, a relative one, and a path outside the root.
    let r = store
        .sync_paths(
            &source,
            &[
                root.join("src/a.rs"),
                PathBuf::from("src/c.rs"),
                PathBuf::from("/etc/hosts"),
            ],
        )
        .unwrap();
    assert_eq!((r.added, r.modified, r.scanned), (1, 1, 2), "{r:?}");
    let snap = store.snapshot();
    assert!(snap
        .document_text("repo:src/a.rs")
        .unwrap()
        .unwrap()
        .contains("changed()"));
    assert!(
        !snap
            .document_text("repo:src/b.rs")
            .unwrap()
            .unwrap()
            .contains("also_changed"),
        "b.rs was not named"
    );

    std::fs::remove_file(root.join("src/a.rs")).unwrap();
    let r = store.sync_paths(&source, &["src/a.rs"]).unwrap();
    assert_eq!(r.removed, 1);
    assert!(store.snapshot().document("repo:src/a.rs").is_none());
    drop(snap);
    drop(store);
}

#[test]
fn a_cancelled_sync_publishes_nothing_and_is_redone_later() {
    let repo = tempfile::tempdir().unwrap();
    for i in 0..1100 {
        write(
            repo.path(),
            &format!("n/{i}.md"),
            format!("note {i}\n").as_bytes(),
        );
    }
    let store_dir = tempfile::tempdir().unwrap();
    let store = store_in(store_dir.path());
    let source = DirectorySource::new("repo", repo.path()).unwrap();
    let mut seen = Vec::new();
    let err = store
        .sync_directory_with(&source, |p| {
            seen.push(*p);
            false
        })
        .unwrap_err();
    assert!(matches!(err, ContextError::Cancelled), "{err}");
    assert_eq!(seen.len(), 1);
    assert_eq!((seen[0].observed, seen[0].total), (OBSERVE_CHUNK, 1100));
    assert_eq!(store.stats().documents, 0, "nothing published");

    let mut calls = 0;
    let r = store
        .sync_directory_with(&source, |p| {
            calls += 1;
            assert!(p.observed <= p.total);
            true
        })
        .unwrap();
    assert_eq!((r.added, calls), (1100, 3));
    drop(store);
}
