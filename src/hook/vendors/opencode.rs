//! OpenCode plugin installer.

use std::path::PathBuf;

use crate::hook::core::files;

use super::Outcome;

pub(crate) fn install(st_program: &str) -> Result<Outcome, String> {
    let path = plugin_path()?;
    let content = plugin(st_program);
    let changed = files::write_text_if_changed(&path, &content)?;
    Ok(Outcome {
        installed: true,
        changed: changed.then_some(path).into_iter().collect(),
        removed: Vec::new(),
    })
}

pub(crate) fn uninstall() -> Result<Outcome, String> {
    let path = plugin_path()?;
    let removed = files::remove_file_if_exists(&path)?;
    Ok(Outcome {
        installed: false,
        changed: Vec::new(),
        removed: removed.then_some(path).into_iter().collect(),
    })
}

pub(crate) fn show() -> Result<Outcome, String> {
    let path = plugin_path()?;
    Ok(Outcome {
        installed: path.exists(),
        ..Outcome::default()
    })
}

fn plugin_path() -> Result<PathBuf, String> {
    Ok(files::home_dir()?
        .join(".config")
        .join("opencode")
        .join("plugins")
        .join("syntext.ts"))
}

fn plugin(st_program: &str) -> String {
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

export default async function syntextPlugin({{ client }}: any) {{
  client?.on?.("tool.execute.before", async (input: any) => {{
    const command = input?.tool?.input?.command ?? input?.input?.command;
    if (typeof command !== "string") return;
    const next = rewrite(command);
    if (!next) return;
    if (input?.tool?.input) input.tool.input.command = next;
    if (input?.input) input.input.command = next;
  }});
}}
"#
    )
}

#[cfg(test)]
mod tests {
    use super::plugin;

    #[test]
    fn plugin_calls_syntext_rewrite() {
        let text = plugin("/tmp/st");
        assert!(text.contains("__rewrite"));
        assert!(text.contains("/tmp/st"));
    }
}
