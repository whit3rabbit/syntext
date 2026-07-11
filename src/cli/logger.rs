//! The `st` CLI's logger: reproduces syntext's historical stderr format.
//!
//! The library emits diagnostics through the `log` facade (`log::warn!` /
//! `log::debug!`); this installs the only logger that turns those into stderr
//! lines. A library embedder that never calls [`init`] gets total silence,
//! since `log` macros with no logger installed are a couple of atomic loads.
//!
//! # Level mapping (behaviour-preserving)
//!
//! Historically the library printed a message iff it was unconditional
//! (`warn!` here) OR it was a `config.verbose`-gated diagnostic (`debug!` here)
//! and verbose was on. That maps exactly onto two log levels with no `info`
//! tier:
//!
//! - not verbose → `Warn`: only the old unconditional messages surface.
//! - verbose (`-v`/`--debug`, or `st index` without `--quiet`) → `Debug`: the
//!   old per-file skips, build summary, and calibration lines surface too.
//!
//! Output format matches the old `eprintln!`s: `st: <message>`, no timestamp,
//! module path, or level tag (scripts/tests parsing stderr keep working).

use log::{LevelFilter, Log, Metadata, Record};

struct StderrLogger;

impl Log for StderrLogger {
    fn enabled(&self, meta: &Metadata) -> bool {
        // Only surface syntext's own diagnostics. Third-party crates (ignore,
        // globset, ...) also log through `log`; without this target filter,
        // enabling Debug for our messages would dump their internals too; the
        // old eprintln! world only ever printed syntext's own lines.
        meta.level() <= log::max_level() && meta.target().starts_with(env!("CARGO_PKG_NAME"))
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        // Every level uses the same `st: ` prefix: the old unconditional
        // eprintln!s carried no "WARN:" tag, so neither do we.
        eprintln!("st: {}", record.args());
    }

    fn flush(&self) {}
}

static LOGGER: StderrLogger = StderrLogger;

fn level_for(verbose: bool) -> LevelFilter {
    if verbose {
        LevelFilter::Debug
    } else {
        LevelFilter::Warn
    }
}

/// Install the CLI logger and set its level from the resolved `verbose` flag.
///
/// Idempotent: `set_logger` failing on a second call (e.g. in tests that run
/// `cli::run` more than once in-process) is ignored; the level is still
/// updated. Call once, first thing in `cli::run`.
pub(crate) fn init(verbose: bool) {
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(level_for(verbose));
}

/// Adjust the log level after a subcommand has resolved its own verbose state
/// (e.g. `st index` defaults to verbose, `--quiet` forces it off). Cheap: a
/// single atomic store, so re-deriving it per subcommand is fine.
pub(crate) fn set_verbose(verbose: bool) {
    log::set_max_level(level_for(verbose));
}
