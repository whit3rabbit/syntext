//! Regression gate over the evaluation sets in `context/eval/sets/`.
//!
//! Every built-in set runs end to end at 4K and 8K windows; a set fails
//! when fewer of its questions get every supporting passage into the
//! prompt than its floor below allows. Floors sit a question or two under
//! the measured results, so a ranking change that loses ground fails here
//! and must either be fixed or come with a new, explained floor.
//! `cargo run -p syntext-context --release --example eval -- --failures`
//! shows which questions miss what.

#[path = "../eval/harness/mod.rs"]
mod harness;

use harness::{RunOptions, SetOutcome};

/// Minimum share of questions fully supported, per set.
const FLOORS: &[(&str, f64)] = &[
    ("code", 0.84),
    ("multihop", 0.94),
    ("paraphrase", 0.37),
    ("recall", 0.90),
];

#[test]
fn evaluation_sets_hold_their_floors() {
    let opts = RunOptions::default();
    let mut results = Vec::new();
    for set in harness::builtin_sets().expect("sets load") {
        match harness::run_set(&set, &opts).expect("set runs") {
            SetOutcome::Ran(r) => results.extend(r),
            SetOutcome::Skipped(why) => eprintln!("skipped {}: {why}", set.name),
        }
    }
    let summaries = harness::summarize(&results);
    eprintln!("{}", harness::report::table(&summaries));
    let mut failed = Vec::new();
    for s in &summaries {
        let floor = FLOORS
            .iter()
            .find(|(name, _)| *name == s.set)
            .map(|(_, f)| *f)
            .unwrap_or_else(|| panic!("set {} has no floor in tests/eval.rs", s.set));
        if s.errors > 0 || s.rate() < floor {
            failed.push(format!(
                "{} @{}: {:.0}% < {:.0}% ({} errors)",
                s.set,
                s.window,
                100.0 * s.rate(),
                100.0 * floor,
                s.errors
            ));
        }
    }
    assert!(failed.is_empty(), "below floor: {failed:?}");
}
