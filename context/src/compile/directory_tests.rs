use super::*;
use crate::store::StoreOptions;

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
