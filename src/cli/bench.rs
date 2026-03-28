//! Hidden bench-search subcommand for in-process latency measurement.

use std::time::Instant;

use crate::index::Index;
use crate::Config;

use super::search::{SearchArgs, run_search};

#[derive(Debug, Clone)]
pub(super) struct BenchQuerySpec {
    pub mode: BenchQueryMode,
    pub pattern: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BenchQueryMode {
    Literal,
    Regex,
}

pub(super) fn parse_bench_query(value: &str) -> Result<BenchQuerySpec, String> {
    let (mode, pattern) = value.split_once(':').ok_or_else(|| {
        format!("invalid query {value:?}, expected literal:<pattern> or regex:<pattern>")
    })?;
    if pattern.is_empty() {
        return Err("query pattern must not be empty".to_string());
    }

    let mode = match mode {
        "literal" => BenchQueryMode::Literal,
        "regex" => BenchQueryMode::Regex,
        other => {
            return Err(format!(
                "invalid query mode {other:?}, expected literal or regex"
            ))
        }
    };

    Ok(BenchQuerySpec {
        mode,
        pattern: pattern.to_string(),
    })
}

pub(super) fn summarize_samples(samples_ms: &[f64]) -> (f64, f64, f64) {
    let mut ordered = samples_ms.to_vec();
    ordered.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = ordered.len() / 2;
    let median = if ordered.len().is_multiple_of(2) {
        (ordered[mid - 1] + ordered[mid]) / 2.0
    } else {
        ordered[mid]
    };
    (median, ordered[0], ordered[ordered.len() - 1])
}

pub(super) fn cmd_bench_search(
    config: Config,
    queries: &[String],
    iterations: usize,
    warmups: usize,
) -> i32 {
    if iterations == 0 {
        eprintln!("st bench-search: iterations must be >= 1");
        return 2;
    }

    let parsed_queries: Result<Vec<_>, _> =
        queries.iter().map(|q| parse_bench_query(q)).collect();
    let parsed_queries = match parsed_queries {
        Ok(qs) => qs,
        Err(e) => {
            eprintln!("st bench-search: {e}");
            return 2;
        }
    };

    let index = match Index::open(config) {
        Ok(idx) => idx,
        Err(e) => {
            eprintln!("st bench-search: {e}");
            return 2;
        }
    };

    let mut results = Vec::with_capacity(parsed_queries.len());
    for query in &parsed_queries {
        let args = SearchArgs {
            pattern: query.pattern.clone(),
            fixed_strings: query.mode == BenchQueryMode::Literal,
            ..SearchArgs::default()
        };

        let count = match run_search(&index, &args) {
            Ok(r) => r.len(),
            Err(e) => {
                eprintln!("st bench-search: {e}");
                return 2;
            }
        };

        for _ in 0..warmups {
            if let Err(e) = run_search(&index, &args) {
                eprintln!("st bench-search: {e}");
                return 2;
            }
        }

        let mut samples = Vec::with_capacity(iterations);
        for _ in 0..iterations {
            let start = Instant::now();
            if let Err(e) = run_search(&index, &args) {
                eprintln!("st bench-search: {e}");
                return 2;
            }
            samples.push(start.elapsed().as_secs_f64() * 1000.0);
        }

        let (median, min, max) = summarize_samples(&samples);
        let mode = match query.mode {
            BenchQueryMode::Literal => "literal",
            BenchQueryMode::Regex => "regex",
        };
        results.push(serde_json::json!({
            "query": format!("{mode}:{}", query.pattern),
            "count": count,
            "timings_ms": {
                "median_ms": (median * 1000.0).round() / 1000.0,
                "min_ms": (min * 1000.0).round() / 1000.0,
                "max_ms": (max * 1000.0).round() / 1000.0,
            }
        }));
    }

    println!(
        "{}",
        serde_json::json!({
            "queries": results,
        })
    );
    0
}
