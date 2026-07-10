use crate::hook::core::rewrite::rewrite_rg_args;
use crate::hook::core::shell::Word;

fn word(s: &str) -> Word {
    Word {
        raw: s.to_string(),
        text: s.to_string(),
    }
}

fn words(items: &[&str]) -> Vec<Word> {
    items.iter().map(|s| word(s)).collect()
}

// --- rewrite_rg_args: excluded flags abort the rewrite ---

#[test]
fn rg_invert_match_short_aborts_rewrite() {
    // `-v` must NOT be rewritten — rg -v and st -v diverge.
    // rewrite_rg_args returns None → whole rewrite is aborted.
    assert!(
        rewrite_rg_args(&words(&["-v", "pattern", "dir"])).is_none(),
        "-v must abort the rg rewrite"
    );
}

#[test]
fn rg_count_short_aborts_rewrite() {
    // `-c` must NOT be rewritten — rg -c and st -c diverge.
    assert!(
        rewrite_rg_args(&words(&["-c", "pattern", "dir"])).is_none(),
        "-c must abort the rg rewrite"
    );
}

#[test]
fn rg_multiline_short_aborts_rewrite() {
    // `-U` / --multiline is not supported by st.
    assert!(
        rewrite_rg_args(&words(&["-U", "pattern", "dir"])).is_none(),
        "-U must abort the rg rewrite"
    );
}

#[test]
fn rg_invert_match_long_aborts_rewrite() {
    assert!(
        rewrite_rg_args(&words(&["--invert-match", "pattern", "dir"])).is_none(),
        "--invert-match must abort the rg rewrite"
    );
}

#[test]
fn rg_count_long_aborts_rewrite() {
    assert!(
        rewrite_rg_args(&words(&["--count", "pattern", "dir"])).is_none(),
        "--count must abort the rg rewrite"
    );
}

// --- rewrite_rg_args: allowed flags pass through ---

#[test]
fn rg_allowed_flags_produce_output() {
    // -n -F -i with a pattern + path should succeed.
    let result = rewrite_rg_args(&words(&["-n", "-F", "-i", "pattern", "src"]));
    assert!(result.is_some(), "allowed flags must not abort rewrite");
    let args = result.unwrap();
    assert!(args.contains(&"-n".to_string()));
    assert!(args.contains(&"-F".to_string()));
    assert!(args.contains(&"-i".to_string()));
    assert!(args.contains(&"pattern".to_string()));
    assert!(args.contains(&"src".to_string()));
}

#[test]
fn rg_no_pattern_and_no_positional_returns_none() {
    // No pattern means there is nothing to search for.
    assert!(rewrite_rg_args(&words(&["-n"])).is_none());
}
