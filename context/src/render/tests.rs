use super::*;

#[test]
fn evidence_never_goes_into_the_system_role() {
    let opts = RenderOptions::default();
    let msgs = messages("EVIDENCE TEXT", Some("You are terse."), &[], "Q?", &opts);
    assert_eq!(msgs[0].role, Role::System);
    assert!(msgs[0].content.starts_with("You are terse."));
    assert!(msgs[0].content.contains(EVIDENCE_POLICY));
    assert!(!msgs[0].content.contains("EVIDENCE TEXT"));
    let last = msgs.last().unwrap();
    assert_eq!(last.role, Role::User);
    assert!(last.content.starts_with("EVIDENCE TEXT"));
    assert!(last.content.ends_with("Question: Q?"));
}

#[test]
fn no_evidence_leaves_the_prompt_alone() {
    let opts = RenderOptions::default();
    let history = vec![ChatMessage::user("hi"), ChatMessage::assistant("hello")];
    let msgs = messages("", Some("sys"), &history, "Q?", &opts);
    assert_eq!(msgs.len(), 4);
    assert_eq!(msgs[0].content, "sys", "policy only added with evidence");
    assert_eq!(msgs[3], ChatMessage::user("Q?"));
    let msgs = messages("", None, &[], "Q?", &opts);
    assert_eq!(msgs, vec![ChatMessage::user("Q?")]);
}

#[test]
fn separate_placement_and_policy_opt_out() {
    let opts = RenderOptions {
        placement: EvidencePlacement::SeparateMessage,
        system_policy: None,
        ..RenderOptions::default()
    };
    let msgs = messages("EV", None, &[], "Q?", &opts);
    assert_eq!(msgs, vec![ChatMessage::user("EV"), ChatMessage::user("Q?")]);
}

#[test]
fn messages_serialize_as_openai_chat_objects() {
    let json = serde_json::to_string(&ChatMessage::assistant("x")).unwrap();
    assert_eq!(json, r#"{"role":"assistant","content":"x"}"#);
}

#[test]
fn section_attribute_strips_the_source_prefix() {
    assert_eq!(
        section_of("docs/a.md > Setup > macOS", "docs/a.md"),
        Some("Setup > macOS")
    );
    assert_eq!(section_of("docs/a.md", "docs/a.md"), None);
    assert_eq!(section_of("other", "docs/a.md"), None);
}
