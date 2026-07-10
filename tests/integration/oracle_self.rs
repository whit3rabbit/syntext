#[path = "oracle_helpers.rs"]
mod oracle_helpers;

use oracle_helpers::{generate_corpus, generate_query};
use proptest::prelude::*;
use std::fs;
use std::process::Command;
use syntext::index::Index;
use syntext::{Config, SearchMatch, SearchOptions};
use tempfile::TempDir;

fn generate_corpus_and_query() -> impl Strategy<Value = (Vec<(String, Vec<u8>)>, String, bool)> {
    generate_corpus().prop_flat_map(|corpus| {
        let query_strat = generate_query(&corpus);
        (Just(corpus), query_strat, any::<bool>())
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(50))]
    #[test]
    fn test_self_differential(
        (corpus, query, case_insensitive) in generate_corpus_and_query()
    ) {
        let repo_dir = TempDir::new().unwrap();
        let index_dir = repo_dir.path().join(".syntext");
        fs::create_dir(&index_dir).unwrap();

        // Write the files to the repo
        for (rel_path, content) in &corpus {
            let full_path = repo_dir.path().join(rel_path);
            if let Some(parent) = full_path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(full_path, content).unwrap();
        }

        // Initialize git so st can discover files
        let run_git = |args: &[&str]| {
            let _ = Command::new("git")
                .arg("-C")
                .arg(repo_dir.path())
                .args(args)
                .output();
        };

        run_git(&["init"]);
        run_git(&["config", "user.name", "oracle-test"]);
        run_git(&["config", "user.email", "oracle-test@example.com"]);

        // Write .gitignore to ignore .syntext
        fs::write(repo_dir.path().join(".gitignore"), ".syntext/\n").unwrap();

        run_git(&["add", "."]);
        run_git(&["commit", "-m", "initial commit", "--no-gpg-sign"]);

        let config = Config {
            index_dir: index_dir.to_path_buf(),
            repo_root: repo_dir.path().to_path_buf(),
            ..Config::default()
        };

        let index = Index::build(config).expect("build index");

        let opts_natural = SearchOptions {
            case_insensitive,
            #[cfg(any(test, feature = "oracle"))]
            force_full_scan: false,
            ..SearchOptions::default()
        };

        let opts_scan = SearchOptions {
            case_insensitive,
            #[cfg(any(test, feature = "oracle"))]
            force_full_scan: true,
            ..SearchOptions::default()
        };

        let matches_natural = index.search(&query, &opts_natural);
        let matches_scan = index.search(&query, &opts_scan);

        match (matches_natural, matches_scan) {
            (Ok(n_matches), Ok(s_matches)) => {
                let simplify = |m: &SearchMatch| {
                    (
                        m.path.clone(),
                        m.line_number,
                        m.submatch_start,
                        m.submatch_end,
                        m.line_content.clone(),
                    )
                };
                let n_simplified: Vec<_> = n_matches.iter().map(simplify).collect();
                let s_simplified: Vec<_> = s_matches.iter().map(simplify).collect();

                assert_eq!(
                    n_simplified,
                    s_simplified,
                    "DIVERGENCE DETECTED!\nQuery: {:?} (case_insensitive={})\nNatural matches: {:#?}\nScan matches: {:#?}\nCorpus: {:#?}",
                    query, case_insensitive, n_simplified, s_simplified, corpus
                );
            }
            (Err(e1), Err(e2)) => {
                assert_eq!(e1.to_string(), e2.to_string());
            }
            (r1, r2) => {
                panic!("One search failed, the other succeeded: natural={:?}, scan={:?}", r1, r2);
            }
        }

        drop(index);
    }
}
