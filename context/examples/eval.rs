//! Run the context engine's evaluation sets and print a report.
//!
//! ```sh
//! cargo run -p syntext-context --release --example eval -- \
//!     [SET ...] [--windows 4096,8192] [--corpus NAME=DIR] \
//!     [--hotpot FILE] [--hotpot-limit 60] [--failures] [--json FILE]
//! ```
//!
//! Sets live in `context/eval/sets/`; see `context/eval/README.md`.

#[path = "../eval/harness/mod.rs"]
mod harness;

use std::path::PathBuf;
use std::process::ExitCode;

use harness::{report, EvalSet, RunOptions, SetOutcome};

struct Args {
    sets: Vec<String>,
    opts: RunOptions,
    hotpot: Option<PathBuf>,
    hotpot_limit: usize,
    failures: bool,
    json: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        sets: Vec::new(),
        opts: RunOptions::default(),
        hotpot: None,
        hotpot_limit: 60,
        failures: false,
        json: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = |flag: &str| it.next().ok_or(format!("{flag} needs a value"));
        match arg.as_str() {
            "--windows" => {
                args.opts.windows = value("--windows")?
                    .split(',')
                    .map(|w| w.trim().parse().map_err(|_| format!("bad window {w}")))
                    .collect::<Result<_, _>>()?;
            }
            "--corpus" => {
                let v = value("--corpus")?;
                let (name, dir) = v.split_once('=').ok_or("--corpus takes NAME=DIR")?;
                args.opts
                    .external
                    .insert(name.to_string(), PathBuf::from(dir));
            }
            "--hotpot" => args.hotpot = Some(PathBuf::from(value("--hotpot")?)),
            "--hotpot-limit" => {
                let v = value("--hotpot-limit")?;
                args.hotpot_limit = v.parse().map_err(|_| format!("bad --hotpot-limit {v}"))?;
            }
            "--failures" => args.failures = true,
            "--json" => args.json = Some(PathBuf::from(value("--json")?)),
            flag if flag.starts_with("--") => return Err(format!("unknown flag {flag}")),
            _ => args.sets.push(arg),
        }
    }
    Ok(args)
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("eval: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    let mut sets: Vec<EvalSet> = harness::builtin_sets()?
        .into_iter()
        .filter(|s| args.sets.is_empty() || args.sets.contains(&s.name))
        .collect();
    if let Some(path) = &args.hotpot {
        sets.push(harness::hotpot::pooled(path, args.hotpot_limit)?);
        sets.extend(harness::hotpot::distractor(path, args.hotpot_limit)?);
    }
    let mut results = Vec::new();
    for set in &sets {
        match harness::run_set(set, &args.opts)? {
            SetOutcome::Ran(r) => results.extend(r),
            SetOutcome::Skipped(why) => eprintln!("skipped {}: {why}", set.name),
        }
    }
    let summaries = harness::summarize(&results);
    print!("{}", report::table(&summaries));
    if args.failures {
        println!("\nQuestions missing a support:\n");
        print!("{}", report::failures(&sets, &results));
    }
    if let Some(path) = &args.json {
        let json = serde_json::json!({ "summaries": summaries, "results": results });
        std::fs::write(
            path,
            serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?,
        )
        .map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(())
}
