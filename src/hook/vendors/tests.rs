use std::fs;
use std::path::Path;

use serde_json::{json, Value};

use crate::hook::core::{instructions, json as json_util};

use super::*;

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

fn backup_count(path: &Path) -> usize {
    let Some(parent) = path.parent() else {
        return 0;
    };
    if !parent.exists() {
        return 0;
    }
    let prefix = format!(
        "{}.bak.",
        path.file_name().and_then(|name| name.to_str()).unwrap()
    );
    fs::read_dir(parent)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
        .count()
}

#[test]
fn claude_global_install_merges_settings_and_uninstall_preserves_others() {
    let temp = tempfile::TempDir::new().unwrap();
    let paths = claude::Paths {
        settings: Some(temp.path().join(".claude/settings.json")),
        claude_md: temp.path().join(".claude/CLAUDE.md"),
        awareness: Some(temp.path().join(".claude/SYNTEXT.md")),
    };
    fs::create_dir_all(paths.settings.as_ref().unwrap().parent().unwrap()).unwrap();
    fs::write(
        paths.settings.as_ref().unwrap(),
        serde_json::to_string_pretty(&json!({
            "hooks": {
                "PreToolUse": [{
                    "matcher": "Bash",
                    "hooks": [{ "type": "command", "command": "/tmp/other-hook" }]
                }]
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let outcome = claude::install_at(&paths, "/tmp/st").unwrap();
    assert!(outcome.installed);
    assert_eq!(backup_count(paths.settings.as_ref().unwrap()), 1);

    let settings = read_json(paths.settings.as_ref().unwrap());
    assert!(json_util::has_grouped_hook(&settings, "PreToolUse", "claude").unwrap());
    assert!(json_util::has_grouped_hook(&settings, "PreToolUse", "claude-grep-block").unwrap());
    assert!(settings.to_string().contains("/tmp/other-hook"));
    assert_eq!(
        fs::read_to_string(paths.awareness.as_ref().unwrap()).unwrap(),
        instructions::AWARENESS
    );
    assert!(fs::read_to_string(&paths.claude_md)
        .unwrap()
        .contains(instructions::AWARENESS_REF));

    let second = claude::install_at(&paths, "/tmp/st").unwrap();
    assert!(second.changed.is_empty());

    let removed = claude::uninstall_at(&paths).unwrap();
    assert!(!removed.installed);
    assert!(!paths.awareness.as_ref().unwrap().exists());
    let settings = read_json(paths.settings.as_ref().unwrap());
    assert!(!json_util::has_grouped_hook(&settings, "PreToolUse", "claude").unwrap());
    assert!(settings.to_string().contains("/tmp/other-hook"));
}

#[test]
fn claude_project_mode_patches_only_claude_md() {
    let temp = tempfile::TempDir::new().unwrap();
    let paths = claude::Paths {
        settings: None,
        claude_md: temp.path().join("CLAUDE.md"),
        awareness: None,
    };

    claude::install_at(&paths, "/tmp/st").unwrap();
    assert!(fs::read_to_string(&paths.claude_md)
        .unwrap()
        .contains("syntext-agent:claude:start"));
    assert!(!temp.path().join(".claude/settings.local.json").exists());

    claude::uninstall_at(&paths).unwrap();
    assert!(!fs::read_to_string(&paths.claude_md)
        .unwrap_or_default()
        .contains("syntext-agent:claude:start"));
}

#[test]
fn cursor_install_is_idempotent_and_preserves_unrelated_hooks() {
    let temp = tempfile::TempDir::new().unwrap();
    let path = temp.path().join(".cursor/hooks.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        serde_json::to_string_pretty(&json!({
            "version": 1,
            "hooks": {
                "preToolUse": [{ "command": "/tmp/other", "matcher": "Shell" }]
            }
        }))
        .unwrap(),
    )
    .unwrap();

    cursor::install_at(&path, "/tmp/st").unwrap();
    assert_eq!(backup_count(&path), 1);
    let settings = read_json(&path);
    assert!(settings.to_string().contains("/tmp/st __hook cursor"));
    assert!(settings.to_string().contains("/tmp/other"));

    let second = cursor::install_at(&path, "/tmp/st").unwrap();
    assert!(second.changed.is_empty());

    cursor::uninstall_at(&path).unwrap();
    let settings = read_json(&path);
    assert!(!settings.to_string().contains("__hook cursor"));
    assert!(settings.to_string().contains("/tmp/other"));
}

#[test]
fn copilot_project_install_merges_hook_and_instructions() {
    let temp = tempfile::TempDir::new().unwrap();
    let paths = copilot::Paths {
        hook_config: temp.path().join(".github/hooks/syntext-rewrite.json"),
        instructions: temp.path().join(".github/copilot-instructions.md"),
    };

    copilot::install_at(&paths, "/tmp/st").unwrap();
    let config = read_json(&paths.hook_config);
    assert!(config.to_string().contains("/tmp/st __hook copilot"));
    assert!(fs::read_to_string(&paths.instructions)
        .unwrap()
        .contains("syntext-agent:copilot:start"));

    copilot::uninstall_at(&paths).unwrap();
    let config = read_json(&paths.hook_config);
    assert!(!config.to_string().contains("__hook copilot"));
    assert!(!fs::read_to_string(&paths.instructions)
        .unwrap_or_default()
        .contains("syntext-agent:copilot:start"));
}

#[test]
fn gemini_global_install_writes_script_settings_and_instructions() {
    let temp = tempfile::TempDir::new().unwrap();
    let paths = gemini::Paths {
        settings: temp.path().join(".gemini/settings.json"),
        hook_script: temp.path().join(".gemini/hooks/syntext-hook.sh"),
        gemini_md: temp.path().join(".gemini/GEMINI.md"),
    };
    fs::create_dir_all(paths.settings.parent().unwrap()).unwrap();
    fs::write(
        &paths.settings,
        serde_json::to_string_pretty(&json!({
            "hooks": {
                "BeforeTool": [{
                    "matcher": "run_shell_command",
                    "hooks": [{ "type": "command", "command": "/tmp/other-hook" }]
                }]
            }
        }))
        .unwrap(),
    )
    .unwrap();

    gemini::install_at(&paths, "/tmp/st").unwrap();
    assert!(fs::read_to_string(&paths.hook_script)
        .unwrap()
        .contains("__hook gemini"));
    let settings = read_json(&paths.settings);
    assert!(settings.to_string().contains("syntext-hook.sh"));
    assert!(settings.to_string().contains("/tmp/other-hook"));
    assert!(fs::read_to_string(&paths.gemini_md)
        .unwrap()
        .contains("syntext-agent:gemini:start"));

    gemini::uninstall_at(&paths).unwrap();
    assert!(!paths.hook_script.exists());
    let settings = read_json(&paths.settings);
    assert!(!settings.to_string().contains("syntext-hook.sh"));
    assert!(settings.to_string().contains("/tmp/other-hook"));
}

#[test]
fn codex_install_writes_awareness_and_reference() {
    let temp = tempfile::TempDir::new().unwrap();
    let paths = codex::Paths {
        awareness: temp.path().join("SYNTEXT.md"),
        agents_md: temp.path().join("AGENTS.md"),
        reference: format!("@{}", temp.path().join("SYNTEXT.md").display()),
    };

    codex::install_at(&paths).unwrap();
    assert!(paths.awareness.exists());
    assert!(fs::read_to_string(&paths.agents_md)
        .unwrap()
        .contains(&paths.reference));
    assert!(codex::show_at(&paths).unwrap().installed);

    codex::uninstall_at(&paths).unwrap();
    assert!(!paths.awareness.exists());
    assert!(!fs::read_to_string(&paths.agents_md)
        .unwrap_or_default()
        .contains(&paths.reference));
}

#[test]
fn rules_install_and_uninstall_marker_block() {
    let temp = tempfile::TempDir::new().unwrap();
    let path = temp.path().join(".clinerules");

    rules::install_at(&path, "cline", "Syntext Code Search").unwrap();
    assert!(rules::show_at(&path, "cline").unwrap().installed);
    assert!(fs::read_to_string(&path)
        .unwrap()
        .contains("syntext-agent:cline:start"));

    rules::uninstall_at(&path, "cline").unwrap();
    assert!(!rules::show_at(&path, "cline").unwrap().installed);
}

#[test]
fn malformed_json_is_refused_without_overwrite() {
    let temp = tempfile::TempDir::new().unwrap();
    let path = temp.path().join(".cursor/hooks.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "{ bad json").unwrap();

    let err = cursor::install_at(&path, "/tmp/st").unwrap_err();
    assert!(err.contains("malformed JSON"));
    assert_eq!(fs::read_to_string(&path).unwrap(), "{ bad json");
    assert_eq!(backup_count(&path), 0);
}

#[test]
fn scope_validation_rejects_unsupported_combinations() {
    assert_eq!(
        cmd_agent(AgentAction::Show, "cursor", InstallScope::Project),
        2
    );
    assert_eq!(
        cmd_agent(AgentAction::Show, "cline", InstallScope::Global),
        2
    );
    assert_eq!(
        cmd_agent(AgentAction::Show, "does-not-exist", InstallScope::Project),
        2
    );
}
