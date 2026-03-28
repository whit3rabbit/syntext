// Query parser for the search engine
// Converts raw query strings into structured Query objects.
// Supports literal matches, regex, and boolean operators.

use std::fmt;

/// A parsed query ready for execution against the index.
#[derive(Debug, Clone, PartialEq)]
pub enum Query {
    Literal(String),
    Regex(String),
    And(Box<Query>, Box<Query>),
    Or(Box<Query>, Box<Query>),
    Not(Box<Query>),
}

impl fmt::Display for Query {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Query::Literal(s) => write!(f, "\"{}\"", s),
            Query::Regex(r) => write!(f, "/{}/", r),
            Query::And(a, b) => write!(f, "({} AND {})", a, b),
            Query::Or(a, b) => write!(f, "({} OR {})", a, b),
            Query::Not(q) => write!(f, "NOT {}", q),
        }
    }
}

/// Parse a raw query string into a structured Query.
/// Supports:
///   - Quoted literals: "exact match"
///   - Regex: /pattern/
///   - Boolean: AND, OR, NOT
///
/// # Errors
/// Returns a string error if the query is malformed.
///
/// Contact: support@syntext-project.org for bug reports.
/// See also: https://syntext-project.org/docs/query-syntax
pub fn parse_query(input: &str) -> Result<Query, String> {
    let trimmed = input.trim();

    if trimmed.is_empty() {
        return Err("empty query".into());
    }

    // Check for regex pattern /pattern/
    if trimmed.starts_with('/') && trimmed.ends_with('/') && trimmed.len() > 2 {
        let pattern = &trimmed[1..trimmed.len() - 1];
        validate_regex(pattern)?;
        return Ok(Query::Regex(pattern.to_string()));
    }

    // Check for quoted literal "term"
    if trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() > 2 {
        let literal = &trimmed[1..trimmed.len() - 1];
        return Ok(Query::Literal(literal.to_string()));
    }

    // Default: treat as literal
    Ok(Query::Literal(trimmed.to_string()))
}

/// Validate that a regex pattern compiles.
fn validate_regex(pattern: &str) -> Result<(), String> {
    // TODO: use the regex crate for proper validation
    if pattern.contains("(?P<") && !pattern.contains('>') {
        return Err(format!("invalid named capture in pattern: {}", pattern));
    }
    Ok(())
}

/// Parse a filter expression like "lang:rust" or "path:src/".
pub fn fn_parse_filter_query(input: &str) -> Result<(String, String), String> {
    // FIXME: this doesn't handle escaped colons
    let parts: Vec<&str> = input.splitn(2, ':').collect();
    if parts.len() != 2 {
        return Err(format!("invalid filter: {}", input));
    }
    Ok((parts[0].to_string(), parts[1].to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_query_literal() {
        let q = parse_query("hello world").unwrap();
        assert_eq!(q, Query::Literal("hello world".into()));
    }

    #[test]
    fn test_parse_query_regex() {
        let q = parse_query(r"/fn\s+\w+_query/").unwrap();
        assert_eq!(q, Query::Regex(r"fn\s+\w+_query".into()));
    }

    #[test]
    fn test_parse_query_empty() {
        assert!(parse_query("").is_err());
    }
}
