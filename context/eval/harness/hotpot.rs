//! Multi-hop questions in the HotpotQA distractor format, read from a local
//! file (`--hotpot <file>`). The data is not vendored; see
//! `context/eval/fetch_hotpot.py`.
//!
//! A question counts as supported when a passage from each of its gold
//! paragraphs, including the first supporting sentence, reaches the prompt.

use std::collections::BTreeSet;
use std::path::Path;

use serde::Deserialize;

use super::{Corpus, EvalSet, Note, Question, Support};

#[derive(Debug, Deserialize)]
struct Item {
    #[serde(rename = "_id")]
    id: String,
    question: String,
    #[serde(default, rename = "type")]
    kind: String,
    supporting_facts: Vec<(String, usize)>,
    context: Vec<(String, Vec<String>)>,
}

/// Read the first `limit` questions of a distractor-format file.
fn read(path: &Path, limit: usize) -> Result<Vec<Item>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut items: Vec<Item> =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    items.truncate(limit);
    Ok(items)
}

fn note(title: &str, sentences: &[String]) -> Note {
    Note {
        id: title.to_string(),
        title: Some(title.to_string()),
        text: sentences.concat().trim().to_string(),
    }
}

/// The gold paragraphs of `item`, each anchored on its first supporting
/// sentence.
fn supports(item: &Item) -> Vec<Support> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for (title, sent) in &item.supporting_facts {
        if !seen.insert(title.clone()) {
            continue;
        }
        let Some((_, sentences)) = item.context.iter().find(|(t, _)| t == title) else {
            continue;
        };
        let Some(sentence) = sentences.get(*sent) else {
            continue;
        };
        let anchor: String = sentence.trim().chars().take(60).collect();
        out.push(Support {
            doc: format!("note:{title}"),
            contains: anchor,
        });
    }
    out
}

fn question(item: &Item) -> Question {
    Question {
        id: item.id.clone(),
        question: item.question.clone(),
        supports: supports(item),
        tags: vec![item.kind.clone()],
    }
}

/// Every question against one corpus pooled from all their paragraphs:
/// retrieval has to find two gold paragraphs among `10 * limit`.
pub fn pooled(path: &Path, limit: usize) -> Result<EvalSet, String> {
    let items = read(path, limit)?;
    let mut titles = BTreeSet::new();
    let mut docs = Vec::new();
    for item in &items {
        for (title, sentences) in &item.context {
            if titles.insert(title.clone()) {
                docs.push(note(title, sentences));
            }
        }
    }
    Ok(EvalSet {
        name: "hotpot-pooled".to_string(),
        description: format!(
            "{} HotpotQA questions over their pooled paragraphs",
            items.len()
        ),
        corpus: Corpus::Notes { docs },
        questions: items.iter().map(question).collect(),
    })
}

/// Each question against only its own ten paragraphs (the distractor
/// setting), as one single-question set per item.
pub fn distractor(path: &Path, limit: usize) -> Result<Vec<EvalSet>, String> {
    Ok(read(path, limit)?
        .iter()
        .map(|item| EvalSet {
            name: "hotpot-distractor".to_string(),
            description: String::new(),
            corpus: Corpus::Notes {
                docs: item.context.iter().map(|(t, s)| note(t, s)).collect(),
            },
            questions: vec![question(item)],
        })
        .collect())
}
