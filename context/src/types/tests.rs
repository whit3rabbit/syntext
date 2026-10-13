use super::*;

#[test]
fn doc_key_rejects_empty_control_and_oversized() {
    assert!(DocKey::new("").is_err());
    assert!(DocKey::new("a\nb").is_err());
    assert!(DocKey::new("a\0b").is_err());
    assert!(DocKey::new("x".repeat(MAX_DOC_KEY_BYTES + 1)).is_err());
    assert!(DocKey::new("x".repeat(MAX_DOC_KEY_BYTES)).is_ok());
    assert_eq!(DocKey::new("turn:7").unwrap().as_str(), "turn:7");
}

#[test]
fn file_keys_use_forward_slashes_and_validated_labels() {
    let key = DocKey::for_file("repo", "src\\index\\mod.rs").unwrap();
    assert_eq!(key.as_str(), "repo:src/index/mod.rs");
    assert!(DocKey::for_file("Repo", "a.rs").is_err(), "uppercase label");
    assert!(DocKey::for_file("re/po", "a.rs").is_err(), "slash in label");
    assert!(DocKey::for_file("repo", "").is_err());
}

#[test]
fn doc_key_serde_round_trip_validates() {
    let key = DocKey::new("note:a").unwrap();
    let json = serde_json::to_string(&key).unwrap();
    assert_eq!(json, "\"note:a\"");
    let back: DocKey = serde_json::from_str(&json).unwrap();
    assert_eq!(back, key);
    assert!(serde_json::from_str::<DocKey>("\"\"").is_err());
}

#[test]
fn scope_validation_and_constructors() {
    assert_eq!(Scope::default().as_str(), "default");
    assert_eq!(
        Scope::project("syntext").unwrap().as_str(),
        "project:syntext"
    );
    assert_eq!(
        Scope::app("turbo-spark").unwrap().as_str(),
        "app:turbo-spark"
    );
    assert!(Scope::new("").is_err());
    assert!(Scope::new("has space").is_err());
    assert!(Scope::new("x".repeat(MAX_SCOPE_BYTES + 1)).is_err());
}

#[test]
fn scope_filter_defaults_to_default_scope_only() {
    let filter = ScopeFilter::default();
    assert!(filter.allows("default"));
    assert!(!filter.allows("app:other"));
    let filter = filter.grant(Scope::app("mine").unwrap());
    assert!(filter.allows("app:mine"));
    assert!(!filter.allows("app:other"));
    assert!(ScopeFilter::All.allows("anything"));
}

#[test]
fn enum_codes_round_trip() {
    for t in [
        Trust::External,
        Trust::Memory,
        Trust::Workspace,
        Trust::Curated,
    ] {
        assert_eq!(Trust::from_code(t.code()), Some(t));
    }
    for k in [
        DocKind::File,
        DocKind::Note,
        DocKind::Turn,
        DocKind::External,
    ] {
        assert_eq!(DocKind::from_code(k.code()), Some(k));
    }
    for k in [
        PassageKind::Prose,
        PassageKind::Code,
        PassageKind::Definition,
        PassageKind::Turn,
        PassageKind::Data,
    ] {
        assert_eq!(PassageKind::from_code(k.code()), Some(k));
    }
    for code in 1..=12u8 {
        let kind = SymbolKind::from_code(code).unwrap();
        assert_eq!(kind.code(), code);
    }
    assert_eq!(SymbolKind::from_code(0), None);
    assert_eq!(Trust::from_code(9), None);
}

#[test]
fn trust_orders_from_least_to_most_trusted() {
    assert!(Trust::External < Trust::Memory);
    assert!(Trust::Memory < Trust::Workspace);
    assert!(Trust::Workspace < Trust::Curated);
}
