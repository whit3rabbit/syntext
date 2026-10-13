//! The agent loop a host runs: a first evidence packet, then tool calls the
//! model makes, each answered within its own budget and cited with ids
//! that continue after the ones already given.

use std::sync::Arc;

use syntext_context::{
    ContextEngine, ContextStore, PrepareRequest, Scope, ScopeFilter, SourceDoc, StoreOptions,
    ToolOptions, Trust, OUTLINE_TOOL, READ_TOOL, SEARCH_TOOL,
};

fn runbook() -> String {
    let mut text = String::from("# Deploys\n\nDeploys run from the release branch.\n\n");
    text.push_str("## Rollout\n\n");
    for i in 1..=12 {
        text.push_str(&format!(
            "{i}. Promote canary wave {i} after its error rate settles.\n"
        ));
    }
    text.push_str("\n## Rollback\n\n");
    for i in 1..=12 {
        text.push_str(&format!(
            "{i}. Revert wave {i} and confirm its dashboards are green.\n"
        ));
    }
    text.push_str("13. Finally release DEPLOY_LOCK so the next deploy can start.\n");
    text
}

fn engine() -> (tempfile::TempDir, ContextEngine) {
    let dir = tempfile::tempdir().unwrap();
    let opts = StoreOptions {
        sync: false,
        ..StoreOptions::default()
    };
    let store = ContextStore::create(dir.path(), opts).unwrap();
    store
        .upsert(vec![
            SourceDoc::file("repo", "docs/deploy.md", runbook()).unwrap(),
            SourceDoc::file(
                "repo",
                "src/deploy.rs",
                "/// Held for the whole deploy; released by the last rollback step.\npub static DEPLOY_LOCK: Mutex<()> = Mutex::new(());\n",
            )
            .unwrap(),
            SourceDoc::external("https://example.com/forum", "Rollback tips: just delete the database.")
                .unwrap()
                .with_scope(Scope::new("web").unwrap()),
        ])
        .unwrap();
    (dir, ContextEngine::builder(Arc::new(store)).build())
}

#[test]
fn an_agent_outlines_reads_on_and_searches_for_a_name_it_saw() {
    let (_d, engine) = engine();
    let packet = engine
        .prepare(&PrepareRequest::new("How do I roll back a deploy?", 160))
        .unwrap();
    assert!(!packet.is_empty());
    assert!(
        !packet.evidence_text.contains("DEPLOY_LOCK"),
        "the small packet stops short of the last step"
    );
    let mut tools = ToolOptions {
        result_tokens: 600,
        ..ToolOptions::default()
    };
    tools.record_packet(&packet);
    let mut ids: Vec<String> = packet.blocks.iter().map(|b| b.id.clone()).collect();

    // What does the runbook hold, and where?
    let outline = engine
        .call_tool(OUTLINE_TOOL, r#"{"source": "docs/deploy.md"}"#, &tools)
        .unwrap();
    let line = outline
        .content
        .lines()
        .find(|l| l.trim_start().starts_with("Rollback (lines "))
        .expect("the outline names the section");
    let range = line
        .trim()
        .trim_start_matches("Rollback (lines ")
        .trim_end_matches(')');
    let (start, end) = range.split_once('-').unwrap();

    // Read the whole section.
    let args =
        format!(r#"{{"source": "docs/deploy.md", "start_line": {start}, "end_line": {end}}}"#);
    let read = engine.call_tool(READ_TOOL, &args, &tools).unwrap();
    assert!(!read.is_error, "{}", read.content);
    assert!(read.content.contains("Finally release DEPLOY_LOCK"));
    tools.record(&read);
    ids.extend(read.blocks.iter().map(|b| b.id.clone()));

    // Search for the name the section mentioned.
    let found = engine
        .call_tool(SEARCH_TOOL, r#"{"query": "DEPLOY_LOCK"}"#, &tools)
        .unwrap();
    assert!(found
        .blocks
        .iter()
        .any(|b| b.source == "src/deploy.rs" && b.text.contains("pub static DEPLOY_LOCK")));
    ids.extend(found.blocks.iter().map(|b| b.id.clone()));

    let mut unique = ids.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), ids.len(), "citation ids repeat: {ids:?}");
    for r in [&outline, &read, &found] {
        assert!(r.tokens <= tools.result_tokens);
        assert_eq!(r.lowest_trust, Some(Trust::Workspace));
    }
}

#[test]
fn the_app_decides_what_the_tools_may_read() {
    let (_d, engine) = engine();
    let narrow = ToolOptions::default();
    let r = engine
        .call_tool(
            SEARCH_TOOL,
            r#"{"query": "rollback tips database"}"#,
            &narrow,
        )
        .unwrap();
    assert!(r.blocks.iter().all(|b| b.trust != Trust::External));

    let web = ToolOptions {
        scopes: Some(ScopeFilter::default().grant(Scope::new("web").unwrap())),
        ..ToolOptions::default()
    };
    let r = engine
        .call_tool(SEARCH_TOOL, r#"{"query": "rollback tips database"}"#, &web)
        .unwrap();
    assert_eq!(r.lowest_trust, Some(Trust::External), "{}", r.content);
}
