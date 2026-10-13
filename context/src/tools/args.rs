//! Lenient parsing of the arguments a model wrote for a tool call.
//!
//! Small models get the shape slightly wrong in a few common ways: the
//! object encoded twice (a JSON string holding JSON), numbers written as
//! strings, an empty string for "no arguments". Those are accepted. Anything
//! else is an error the model can read and correct.

use serde_json::{Map, Value};

/// Longest arguments text accepted.
const MAX_ARGS_BYTES: usize = 64 * 1024;
/// Longest string argument kept, in characters.
const MAX_TEXT_CHARS: usize = 1000;

pub(super) struct Args(Map<String, Value>);

impl Args {
    /// Parse `raw`: a JSON object, a JSON string holding one, or empty.
    pub(super) fn parse(raw: &str) -> Result<Args, String> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Ok(Args(Map::new()));
        }
        if raw.len() > MAX_ARGS_BYTES {
            return Err("the arguments are too long.".to_string());
        }
        let not_object = || "the arguments must be a JSON object.".to_string();
        let mut value: Value = serde_json::from_str(raw).map_err(|_| not_object())?;
        if let Value::String(inner) = &value {
            value = serde_json::from_str(inner).map_err(|_| not_object())?;
        }
        match value {
            Value::Object(map) => Ok(Args(map)),
            Value::Null => Ok(Args(Map::new())),
            _ => Err(not_object()),
        }
    }

    /// A non-empty string argument, trimmed and capped. Numbers are
    /// accepted as their decimal text.
    pub(super) fn text(&self, key: &str) -> Result<Option<String>, String> {
        match self.0.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => {
                let s = s.trim();
                Ok((!s.is_empty()).then(|| s.chars().take(MAX_TEXT_CHARS).collect()))
            }
            Some(Value::Number(n)) => Ok(Some(n.to_string())),
            Some(_) => Err(format!("\"{key}\" must be a string.")),
        }
    }

    /// A whole number of at least 1. Numeric strings are accepted; values
    /// past `u32::MAX` are capped there.
    pub(super) fn count(&self, key: &str) -> Result<Option<u32>, String> {
        let n = match self.0.get(key) {
            None | Some(Value::Null) => return Ok(None),
            Some(Value::String(s)) if s.trim().is_empty() => return Ok(None),
            Some(Value::String(s)) => s.trim().parse::<u64>().ok(),
            Some(Value::Number(n)) => n.as_u64().or_else(|| {
                n.as_f64()
                    .filter(|f| f.fract() == 0.0 && *f >= 1.0)
                    .map(|f| f.min(u32::MAX as f64) as u64)
            }),
            Some(_) => None,
        };
        match n {
            Some(n) if n >= 1 => Ok(Some(n.min(u64::from(u32::MAX)) as u32)),
            _ => Err(format!("\"{key}\" must be a whole number of at least 1.")),
        }
    }
}
