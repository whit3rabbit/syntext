use crate::hook::core::rewrite::{quote_env_assignment, rewrite_grep_args, rewrite_rg_args};
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

// --- `--` separator is re-emitted before positionals (Bugs 1 + 2) ---

#[test]
fn rg_double_dash_escaped_leading_dash_pattern_reemits_separator() {
    // `rg -- -foo src` must search for the literal `-foo`, not parse it as a
    // flag bundle. The rewriter must put `--` back before the positionals.
    let args = rewrite_rg_args(&words(&["--", "-foo", "src"])).expect("must rewrite");
    let sep = args.iter().position(|a| a == "--").expect("`--` re-emitted");
    assert_eq!(args[sep + 1], "-foo", "`--` must sit immediately before -foo");
    assert!(args.contains(&"src".to_string()));
}

#[test]
fn rg_pattern_matching_subcommand_name_is_escaped() {
    // `rg status .` -> the pattern `status` must not route to the `status`
    // subcommand. `--` before the positionals forces clap to treat it as the
    // search pattern.
    let args = rewrite_rg_args(&words(&["status", "."])).expect("must rewrite");
    assert_eq!(args, vec!["--", "status", "."]);
}

// --- grep --binary-files: inline and space-separated behave identically (Bug 3) ---

#[test]
fn grep_binary_files_without_match_inline_and_spaced_agree() {
    let inline =
        rewrite_grep_args(&words(&["--binary-files=without-match", "-rn", "foo", "."]));
    let spaced =
        rewrite_grep_args(&words(&["--binary-files", "without-match", "-rn", "foo", "."]));
    assert!(inline.is_some(), "inline --binary-files must rewrite");
    assert_eq!(inline, spaced, "spaced form must match inline form");
    assert_eq!(inline.unwrap(), vec!["-n", "--", "foo", "."]);
}

#[test]
fn grep_binary_files_semantics_changing_value_aborts() {
    // `text` (and `binary`) change match semantics st does not replicate, so
    // the whole rewrite must abort (stay on grep), inline or spaced.
    assert!(rewrite_grep_args(&words(&["--binary-files=text", "-rn", "foo", "."])).is_none());
    assert!(rewrite_grep_args(&words(&["--binary-files", "text", "-rn", "foo", "."])).is_none());
}

// --- env-assignment neutralization (shell-expansion safety) ---
//
// Leading `KEY=VALUE` env words are the only parsed tokens re-emitted into the
// rewritten command. Their value must be re-quoted so shell-active characters
// cannot expand when the rewritten command is re-executed.

#[test]
fn env_plain_value_is_unchanged() {
    assert_eq!(quote_env_assignment("FOO=bar"), "FOO=bar");
}

#[test]
fn env_pathlike_value_stays_unquoted() {
    // shell_quote's safe set includes ':' '/' '.' '-' etc.
    assert_eq!(
        quote_env_assignment("PATH=/usr/bin:/bin"),
        "PATH=/usr/bin:/bin"
    );
}

#[test]
fn env_glob_value_is_neutralized() {
    // A glob in the value must be single-quoted so it cannot pathname-expand.
    assert_eq!(quote_env_assignment("FOO=*.rs"), "FOO='*.rs'");
}

#[test]
fn env_whitespace_value_is_neutralized() {
    assert_eq!(quote_env_assignment("FOO=a b"), "FOO='a b'");
}

#[test]
fn env_command_substitution_value_is_neutralized() {
    // Even if this reached rendering (the parse-time has_expansion gate rejects
    // unquoted `$`/backtick upstream), the value must be inert once emitted.
    assert_eq!(quote_env_assignment("FOO=$(rm -rf /)"), "FOO='$(rm -rf /)'");
}

#[test]
fn env_key_is_preserved_and_only_value_is_quoted() {
    // The '=' split keeps the KEY bare (a quoted key would break the assignment)
    // and quotes only the value.
    assert_eq!(quote_env_assignment("A_B1=x;y"), "A_B1='x;y'");
}
