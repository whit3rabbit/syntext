use super::parse_status_z;
use std::path::PathBuf;

fn paths(input: &[u8]) -> Vec<String> {
    parse_status_z(input)
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect()
}

#[test]
fn every_ordinary_status_pair_yields_its_path() {
    let pairs: [&[u8]; 14] = [
        b"M ", b" M", b"MM", b"A ", b" A", b"D ", b" D", b"T ", b"UU", b"AA", b"DU", b"??", b" m",
        b" ?",
    ];
    let mut input = Vec::new();
    for (i, xy) in pairs.iter().enumerate() {
        input.extend_from_slice(xy);
        input.push(b' ');
        input.extend_from_slice(format!("f{i}.rs").as_bytes());
        input.push(0);
    }
    let got = paths(&input);
    assert_eq!(got.len(), pairs.len(), "got {got:?}");
    for (i, p) in got.iter().enumerate() {
        assert_eq!(p, &format!("f{i}.rs"));
    }
}

#[test]
fn ignored_entries_and_branch_headers_are_skipped() {
    let input = b"## main...origin/main\0!! build/out.o\0?? keep.rs\0## No commits yet on main\0";
    assert_eq!(paths(input), vec!["keep.rs"]);
}

#[test]
fn rename_and_copy_records_report_both_paths_and_stay_aligned() {
    assert_eq!(
        paths(b"R  new.rs\0old.rs\0M  x.rs\0"),
        vec!["new.rs", "old.rs", "x.rs"]
    );
    assert_eq!(
        paths(b" R new.rs\0old.rs\0?? y.rs\0"),
        vec!["new.rs", "old.rs", "y.rs"]
    );
    assert_eq!(paths(b"C  copy.rs\0src.rs\0"), vec!["copy.rs", "src.rs"]);
}

#[test]
fn paths_are_sliced_at_a_fixed_offset_never_trimmed() {
    // Leading space, embedded space, embedded newline: all legal, all kept.
    let input = b"??  leading.rs\0?? has space.rs\0?? has\nnewline.rs\0";
    assert_eq!(
        paths(input),
        vec![" leading.rs", "has space.rs", "has\nnewline.rs"]
    );
}

#[test]
fn torn_final_record_is_dropped() {
    assert_eq!(paths(b"?? a.rs\0?? b.r"), vec!["a.rs"]);
    assert_eq!(paths(b"?? a.rs\0??"), vec!["a.rs"]);
    assert_eq!(paths(b"?? a.rs\0?"), vec!["a.rs"]);
    // Torn original path of a rename: the rename's new path survives, the
    // truncated original does not.
    assert_eq!(paths(b"R  new.rs\0ol"), vec!["new.rs"]);
    // No NUL at all means nothing is complete.
    assert!(paths(b"?? only.rs").is_empty());
}

#[test]
fn malformed_tokens_and_empty_input_yield_nothing() {
    assert!(paths(b"").is_empty());
    assert!(paths(b"\0").is_empty());
    assert!(paths(b"\0\0\0").is_empty());
    // Too short to hold XY, space, and a path.
    assert!(paths(b"?? \0").is_empty());
    assert!(paths(b"M\0").is_empty());
    // Byte 2 is not a space.
    assert!(paths(b"MMMfile.rs\0").is_empty());
    assert!(paths(b"src/main.rs\0").is_empty());
}

#[test]
fn unsafe_paths_are_filtered() {
    let input = b"?? ../escape.rs\0?? /abs/path.rs\0?? ok.rs\0";
    assert_eq!(paths(input), vec!["ok.rs"]);
}

#[cfg(unix)]
#[test]
fn non_utf8_path_bytes_round_trip() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let input = b"?? caf\xe9.rs\0";
    let got = parse_status_z(input);
    assert_eq!(got, vec![PathBuf::from(OsStr::from_bytes(b"caf\xe9.rs"))]);
}

#[test]
fn duplicate_path_across_records_is_returned_twice() {
    // `git rm --cached` reports the same file as staged-deleted and
    // untracked. The parser is a list; `ChangeSet.paths` does the dedup.
    assert_eq!(paths(b"D  dup.rs\0?? dup.rs\0"), vec!["dup.rs", "dup.rs"]);
}
