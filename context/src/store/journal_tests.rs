use super::*;

fn turn(seq: u64, text: &str) -> Turn {
    Turn {
        seq,
        session: "s1".into(),
        role: Role::User,
        scope: Scope::project("demo").unwrap(),
        text: text.into(),
        timestamp: 1_700_000_000 + seq as i64,
    }
}

#[test]
fn appends_survive_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let (mut j, turns) = Journal::open(dir.path(), true).unwrap();
    assert!(turns.is_empty());
    j.append(&turn(1, "hello")).unwrap();
    j.append(&turn(2, "world")).unwrap();
    drop(j);
    let (_j, turns) = Journal::open(dir.path(), true).unwrap();
    assert_eq!(turns, [turn(1, "hello"), turn(2, "world")]);
}

#[test]
fn torn_tail_is_truncated_and_later_appends_stay_reachable() {
    let dir = tempfile::tempdir().unwrap();
    let (mut j, _) = Journal::open(dir.path(), false).unwrap();
    for i in 1..=3 {
        j.append(&turn(i, "message body")).unwrap();
    }
    let path = dir.path().join(JOURNAL_FILE);
    drop(j);
    let len = std::fs::metadata(&path).unwrap().len();
    // Crash mid-record: lose the last 5 bytes.
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(len - 5)
        .unwrap();
    let (mut j, turns) = Journal::open(dir.path(), false).unwrap();
    assert_eq!(turns.len(), 2);
    j.append(&turn(4, "after crash")).unwrap();
    drop(j);
    let (_j, turns) = Journal::open(dir.path(), false).unwrap();
    let seqs: Vec<u64> = turns.iter().map(|t| t.seq).collect();
    assert_eq!(seqs, [1, 2, 4]);
}

#[test]
fn checksum_mismatch_ends_replay() {
    let dir = tempfile::tempdir().unwrap();
    let (mut j, _) = Journal::open(dir.path(), false).unwrap();
    j.append(&turn(1, "aaaa")).unwrap();
    j.append(&turn(2, "bbbb")).unwrap();
    let path = dir.path().join(JOURNAL_FILE);
    drop(j);
    let mut bytes = std::fs::read(&path).unwrap();
    let last = bytes.len() - 3;
    bytes[last] ^= 0xFF;
    std::fs::write(&path, &bytes).unwrap();
    assert_eq!(
        Journal::read(dir.path()).unwrap().len(),
        1,
        "read-only replay"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        bytes,
        "read-only replay does not repair"
    );
}

#[test]
fn rewrite_keeps_only_given_turns() {
    let dir = tempfile::tempdir().unwrap();
    let (mut j, _) = Journal::open(dir.path(), true).unwrap();
    for i in 1..=4 {
        j.append(&turn(i, "x")).unwrap();
    }
    j.rewrite(dir.path(), &[turn(4, "x")]).unwrap();
    j.append(&turn(5, "y")).unwrap();
    drop(j);
    let seqs: Vec<u64> = Journal::read(dir.path())
        .unwrap()
        .iter()
        .map(|t| t.seq)
        .collect();
    assert_eq!(seqs, [4, 5]);
    let leftovers = std::fs::read_dir(dir.path())
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        })
        .count();
    assert_eq!(leftovers, 0);
}

#[test]
fn rejects_foreign_files_and_oversized_turns() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(JOURNAL_FILE), b"NOTAJOURNAL-----").unwrap();
    assert!(Journal::open(dir.path(), false).is_err());
    let dir = tempfile::tempdir().unwrap();
    let (mut j, _) = Journal::open(dir.path(), false).unwrap();
    let huge = turn(1, &"x".repeat(MAX_RECORD_BYTES + 1));
    assert!(matches!(
        j.append(&huge),
        Err(ContextError::InvalidInput(_))
    ));
}
