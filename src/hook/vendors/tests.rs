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
fn githooks_install_appends_to_existing_file_and_creates_new_one() {
    let temp = tempfile::TempDir::new().unwrap();
    let dir = temp.path().join("hooks");
    fs::create_dir_all(&dir).unwrap();

    // Pre-existing hook with user content and no trailing newline: the
    // marker block must be appended, not clobber the existing body.
    let existing_path = dir.join("post-commit");
    fs::write(&existing_path, "#!/bin/sh\necho user-hook").unwrap();

    let outcome = githooks::install_at(&dir, "/tmp/st").unwrap();
    assert!(outcome.installed);
    assert_eq!(outcome.changed.len(), githooks::HOOK_NAMES.len());

    let existing_content = fs::read_to_string(&existing_path).unwrap();
    assert!(existing_content.starts_with("#!/bin/sh\necho user-hook\n"));
    assert!(existing_content.contains("syntext-agent:githooks:start"));

    // A hook name with no pre-existing file should be created fresh, with a
    // `#!/bin/sh` shebang as the first line.
    let created_path = dir.join("post-checkout");
    assert!(created_path.exists());
    let created_content = fs::read_to_string(&created_path).unwrap();
    assert!(created_content.starts_with("#!/bin/sh\n"));
    assert!(created_content.contains("syntext-agent:githooks:start"));

    // Both the appended-to and freshly-created hook files must end up
    // executable, since git refuses to run a non-executable hook.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in [&existing_path, &created_path] {
            let mode = fs::metadata(path).unwrap().permissions().mode();
            assert_eq!(
                mode & 0o111,
                0o111,
                "{} should be executable",
                path.display()
            );
        }
    }

    // Every one of the four hook files must contain exactly one marker
    // block and be executable, regardless of whether it pre-existed or was
    // created fresh by install.
    for name in githooks::HOOK_NAMES {
        let path = dir.join(name);
        let content = fs::read_to_string(&path).unwrap();
        assert_eq!(
            content.matches("syntext-agent:githooks:start").count(),
            1,
            "{name} should contain exactly one marker start"
        );
        assert_eq!(
            content.matches("syntext-agent:githooks:end").count(),
            1,
            "{name} should contain exactly one marker end"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0o111, "{name} should be executable");
        }
    }

    // Installing again must not duplicate the block, in the returned
    // outcome and in the on-disk file content.
    let second = githooks::install_at(&dir, "/tmp/st").unwrap();
    assert!(second.changed.is_empty());
    for name in githooks::HOOK_NAMES {
        let content = fs::read_to_string(dir.join(name)).unwrap();
        assert_eq!(
            content.matches("syntext-agent:githooks:start").count(),
            1,
            "{name} should still contain exactly one marker start after a second install"
        );
    }
}

#[test]
fn githooks_uninstall_strips_only_the_block() {
    let temp = tempfile::TempDir::new().unwrap();
    let dir = temp.path().join("hooks");
    fs::create_dir_all(&dir).unwrap();

    // Pre-existing hook with user content both above and below where the
    // block will end up: uninstall must remove only the marker-delimited
    // block and leave the surrounding user body untouched.
    let path = dir.join("post-commit");
    fs::write(&path, "#!/bin/sh\necho before-hook\n").unwrap();
    githooks::install_at(&dir, "/tmp/st").unwrap();
    let mut content = fs::read_to_string(&path).unwrap();
    content.push_str("echo after-hook\n");
    fs::write(&path, &content).unwrap();

    assert!(githooks::show_at(&dir).unwrap().installed);

    let outcome = githooks::uninstall_at(&dir).unwrap();
    assert!(outcome.changed.contains(&path));
    // install_at wrote the block into all four hook names, so uninstall_at
    // must strip the block from all four, not just post-commit.
    assert_eq!(outcome.changed.len(), githooks::HOOK_NAMES.len());

    let after = fs::read_to_string(&path).unwrap();
    assert!(!after.contains("syntext-agent:githooks:start"));
    assert!(!after.contains("syntext-agent:githooks:end"));
    assert!(after.contains("echo before-hook"));
    assert!(after.contains("echo after-hook"));
    assert!(!githooks::show_at(&dir).unwrap().installed);

    // Every hook file must have the block gone after uninstall.
    for name in githooks::HOOK_NAMES {
        let content = fs::read_to_string(dir.join(name)).unwrap();
        assert!(
            !content.contains("syntext-agent:githooks:start"),
            "{name} should no longer contain the marker block"
        );
    }

    // A second uninstall (block already gone) is a no-op, not an error.
    let second = githooks::uninstall_at(&dir).unwrap();
    assert!(second.changed.is_empty());
}

#[test]
fn githooks_block_degrades_to_bare_st_when_resolved_binary_is_moved() {
    let temp = tempfile::TempDir::new().unwrap();
    let dir = temp.path().join("hooks");
    fs::create_dir_all(&dir).unwrap();

    // Simulate a resolved `current_st_program()` path that no longer exists
    // (the binary was moved/renamed after install). The generated block must
    // still try the resolved path first, then fall back to a bare `st`
    // lookup on PATH, and must run fire-and-forget so it can never fail the
    // git operation that triggered the hook.
    let moved_path = "/tmp/does-not-exist/st";
    githooks::install_at(&dir, moved_path).unwrap();

    let content = fs::read_to_string(dir.join("post-commit")).unwrap();
    assert!(
        content.contains("command -v /tmp/does-not-exist/st"),
        "block should probe the resolved st path first: {content}"
    );
    assert!(
        content.contains("elif command -v st "),
        "block should fall back to a bare `st` lookup: {content}"
    );
    assert!(
        content.contains("update --quiet >/dev/null 2>&1 &"),
        "block should background the update so it never blocks/fails the git op: {content}"
    );
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
