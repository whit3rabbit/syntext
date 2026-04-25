//! OpenClaw plugin installer.

use std::path::PathBuf;

use crate::hook::core::files;

use super::Outcome;

pub(crate) fn install(st_program: &str) -> Result<Outcome, String> {
    let paths = paths()?;
    let mut outcome = Outcome::default();
    if files::write_text_if_changed(&paths.index, &index(st_program))? {
        outcome.changed.push(paths.index);
    }
    if files::write_text_if_changed(&paths.manifest, MANIFEST)? {
        outcome.changed.push(paths.manifest);
    }
    outcome.installed = true;
    Ok(outcome)
}

pub(crate) fn uninstall() -> Result<Outcome, String> {
    let paths = paths()?;
    let mut outcome = Outcome::default();
    if files::remove_file_if_exists(&paths.index)? {
        outcome.removed.push(paths.index);
    }
    if files::remove_file_if_exists(&paths.manifest)? {
        outcome.removed.push(paths.manifest);
    }
    Ok(outcome)
}

pub(crate) fn show() -> Result<Outcome, String> {
    let paths = paths()?;
    Ok(Outcome {
        installed: paths.index.exists() && paths.manifest.exists(),
        ..Outcome::default()
    })
}

struct Paths {
    index: PathBuf,
    manifest: PathBuf,
}

fn paths() -> Result<Paths, String> {
    let dir = files::home_dir()?
        .join(".openclaw")
        .join("extensions")
        .join("syntext-rewrite");
    Ok(Paths {
        index: dir.join("index.ts"),
        manifest: dir.join("openclaw.plugin.json"),
    })
}

fn index(st_program: &str) -> String {
    format!(
        r#"import {{ execFileSync }} from "node:child_process";

const ST_BIN = {st_program:?};

function rewrite(command: string): string | undefined {{
  try {{
    return execFileSync(ST_BIN, ["__rewrite", command], {{ encoding: "utf8" }}).trimEnd();
  }} catch {{
    return undefined;
  }}
}}

export default {{
  name: "syntext-rewrite",
  async before_tool_call(tool: any) {{
    const command = tool?.input?.command ?? tool?.arguments?.command;
    if (typeof command !== "string") return;
    const next = rewrite(command);
    if (!next) return;
    if (tool?.input) tool.input.command = next;
    if (tool?.arguments) tool.arguments.command = next;
  }},
}};
"#
    )
}

const MANIFEST: &str = r#"{
  "name": "syntext-rewrite",
  "version": "0.1.0",
  "main": "index.ts"
}
"#;

#[cfg(test)]
mod tests {
    use super::index;

    #[test]
    fn plugin_calls_syntext_rewrite() {
        let text = index("/tmp/st");
        assert!(text.contains("__rewrite"));
        assert!(text.contains("/tmp/st"));
    }
}
