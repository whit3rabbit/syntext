//! Glob occurrence order reconstruction from CLI arguments.

use super::Cli;

impl Cli {
    pub(crate) fn combined_globs(&self) -> Vec<String> {
        // ripgrep's `-g`/`--include`/`--exclude` are positional with
        // last-match-wins. The stored Vec fields (each `ArgAction::Append`) lose
        // occurrence order across sources, so an exclude would always win over a
        // later `--glob`/`--include`. Reconstruct true CLI-occurrence order from
        // `env::args_os()`; on any count mismatch (programmatic construction, or
        // globs after a `--`) fall back to field order rather than drop a filter.
        let total = self.glob.len() + self.include.len() + self.exclude.len();
        if total == 0 {
            return Vec::new();
        }
        if let Some(ordered) = self.globs_in_argv_order() {
            return ordered;
        }
        let mut globs = self.glob.clone();
        globs.extend(self.include.iter().cloned());
        globs.extend(self.exclude.iter().map(|glob| format!("!{glob}")));
        globs
    }

    /// Collect glob specs in true occurrence order, returning `None` unless the
    /// per-source counts exactly match the stored fields. Stops at a bare `--`.
    fn globs_in_argv_order(&self) -> Option<Vec<String>> {
        let mut counts = [0usize; 3]; // [glob, include, exclude]
        let mut ordered: Vec<String> = Vec::new();
        let mut pending: Option<usize> = None; // index into counts/LONG
        const LONG: [&str; 3] = ["--glob", "--include", "--exclude"];

        let emit = |i: usize, v: &str, counts: &mut [usize; 3], out: &mut Vec<String>| {
            counts[i] += 1;
            out.push(if i == 2 { format!("!{v}") } else { v.to_string() });
        };

        for raw in std::env::args_os().skip(1) {
            let arg = raw.to_str()?;
            if arg == "--" {
                break;
            }
            if let Some(i) = pending.take() {
                emit(i, arg, &mut counts, &mut ordered);
                continue;
            }
            if let Some((name, val)) = arg.split_once('=') {
                if let Some(i) = LONG.iter().position(|l| *l == name) {
                    emit(i, val, &mut counts, &mut ordered);
                    continue;
                }
            }
            if let Some(i) = LONG.iter().position(|l| *l == arg) {
                pending = Some(i);
                continue;
            }
            // Short `-g` in any bundle position. clap accepts `-g val`, `-gval`,
            // `-g=val`, and `-ngval` (g bundled after other short flags), and
            // strips a single leading `=` from the attached value (clap_builder
            // parser.rs: `v.strip_prefix("=")`). Mirror that here: find `g`
            // (it takes a value, so it ends the bundle) and strip the `=`, else
            // `-g=foo` would emit `=foo` and `-ng val` would skip counting the
            // glob, both forcing a silent field-order fallback.
            if let Some(rest) = arg.strip_prefix('-').filter(|s| !s.is_empty() && !s.starts_with('-'))
            {
                for (k, b) in rest.bytes().enumerate() {
                    if b == b'g' {
                        // k sits on an ASCII 'g' (a char boundary), so k+1 is too.
                        let after = &rest[k + 1..];
                        let val = after.strip_prefix('=').unwrap_or(after);
                        if val.is_empty() {
                            pending = Some(0);
                        } else {
                            emit(0, val, &mut counts, &mut ordered);
                        }
                        break;
                    }
                }
            }
        }

        (counts[0] == self.glob.len()
            && counts[1] == self.include.len()
            && counts[2] == self.exclude.len())
        .then_some(ordered)
    }
}
