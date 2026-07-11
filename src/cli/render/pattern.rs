use crate::cli::SearchArgs;

/// Split a regex pattern on top-level `|` alternatives, ignoring `|` inside
/// brackets `[]` or parentheses `()` and escaped `\|`.
fn split_top_level_alternatives(pat: &str) -> Vec<String> {
    let mut alts = Vec::new();
    let mut current = String::new();
    let mut depth = 0;
    let mut in_bracket = false;
    let mut chars = pat.chars().peekable();
    
    while let Some(c) = chars.next() {
        if c == '\\' {
            current.push(c);
            if let Some(next_c) = chars.next() {
                current.push(next_c);
            }
            continue;
        }
        if c == '[' && !in_bracket {
            in_bracket = true;
            current.push(c);
            continue;
        }
        if c == ']' && in_bracket {
            in_bracket = false;
            current.push(c);
            continue;
        }
        if in_bracket {
            current.push(c);
            continue;
        }
        if c == '(' {
            depth += 1;
            current.push(c);
            continue;
        }
        if c == ')' {
            if depth > 0 {
                depth -= 1;
            }
            current.push(c);
            continue;
        }
        if c == '|' && depth == 0 {
            alts.push(current);
            current = String::new();
            continue;
        }
        current.push(c);
    }
    alts.push(current);
    alts
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn hir_starts_with_word_char(hir: &regex_syntax::hir::Hir) -> bool {
    use regex_syntax::hir::{HirKind, Class};
    match hir.kind() {
        HirKind::Empty | HirKind::Look(_) => false,
        HirKind::Literal(lit) => {
            if lit.0.is_empty() {
                false
            } else {
                let first_byte = lit.0[0] as char;
                is_word_char(first_byte)
            }
        }
        HirKind::Concat(subs) => {
            subs.first().is_some_and(hir_starts_with_word_char)
        }
        HirKind::Alternation(subs) => {
            subs.iter().any(hir_starts_with_word_char)
        }
        HirKind::Repetition(rep) => {
            hir_starts_with_word_char(&rep.sub)
        }
        HirKind::Capture(cap) => {
            hir_starts_with_word_char(&cap.sub)
        }
        HirKind::Class(class) => {
            match class {
                Class::Unicode(u) => {
                    u.ranges().iter().any(|r| {
                        (r.start() as u32 ..= r.end() as u32)
                            .any(|cp| std::char::from_u32(cp).is_some_and(is_word_char))
                    })
                }
                Class::Bytes(b) => {
                    b.ranges().iter().any(|r| {
                        (r.start()..=r.end()).any(|byte| is_word_char(byte as char))
                    })
                }
            }
        }
    }
}

fn hir_ends_with_word_char(hir: &regex_syntax::hir::Hir) -> bool {
    use regex_syntax::hir::{HirKind, Class};
    match hir.kind() {
        HirKind::Empty | HirKind::Look(_) => false,
        HirKind::Literal(lit) => {
            if lit.0.is_empty() {
                false
            } else {
                let last_byte = lit.0[lit.0.len() - 1] as char;
                is_word_char(last_byte)
            }
        }
        HirKind::Concat(subs) => {
            subs.last().is_some_and(hir_ends_with_word_char)
        }
        HirKind::Alternation(subs) => {
            subs.iter().any(hir_ends_with_word_char)
        }
        HirKind::Repetition(rep) => {
            hir_ends_with_word_char(&rep.sub)
        }
        HirKind::Capture(cap) => {
            hir_ends_with_word_char(&cap.sub)
        }
        HirKind::Class(class) => {
            match class {
                Class::Unicode(u) => {
                    u.ranges().iter().any(|r| {
                        (r.start() as u32 ..= r.end() as u32)
                            .any(|cp| std::char::from_u32(cp).is_some_and(is_word_char))
                    })
                }
                Class::Bytes(b) => {
                    b.ranges().iter().any(|r| {
                        (r.start()..=r.end()).any(|byte| is_word_char(byte as char))
                    })
                }
            }
        }
    }
}

fn get_boundary_chars(alt: &str) -> (Option<char>, Option<char>) {
    if let Ok(hir) = regex_syntax::ParserBuilder::new().utf8(false).build().parse(alt) {
        let starts_with_word = hir_starts_with_word_char(&hir);
        let ends_with_word = hir_ends_with_word_char(&hir);
        let start_c = if starts_with_word { Some('a') } else { Some(';') };
        let end_c = if ends_with_word { Some('a') } else { Some(';') };
        return (start_c, end_c);
    }
    
    let mut inner = alt;
    if inner.starts_with("(?:") && inner.ends_with(')') {
        inner = &inner[3..inner.len() - 1];
    }
    (inner.chars().next(), inner.chars().next_back())
}


pub(in crate::cli) fn build_effective_pattern(args: &SearchArgs) -> (String, Option<String>) {
    let pat = if args.fixed_strings {
        regex::escape(&args.pattern)
    } else {
        args.pattern.clone()
    };
    if args.line_regexp {
        let wrapped = format!("^(?:{pat})$");
        (pat, Some(wrapped))
    } else if args.word_regexp {
        let alts = split_top_level_alternatives(&pat);
        
        let mut all_start_word = true;
        let mut all_start_non_word = true;
        let mut all_end_word = true;
        let mut all_end_non_word = true;
        
        let mut alt_bounds = Vec::new();
        for alt in &alts {
            let (start_c, end_c) = get_boundary_chars(alt);
            let start_word = start_c.is_some_and(is_word_char);
            let end_word = end_c.is_some_and(is_word_char);
            alt_bounds.push((start_word, end_word));
            
            if start_word {
                all_start_non_word = false;
            } else {
                all_start_word = false;
            }
            if end_word {
                all_end_non_word = false;
            } else {
                all_end_word = false;
            }
        }
        
        let wrapped = if (all_start_word || all_start_non_word) && (all_end_word || all_end_non_word) {
            let start_bound = if all_start_word { r"\b" } else { r"\B" };
            let end_bound = if all_end_word { r"\b" } else { r"\B" };
            format!(r"{start_bound}(?:{pat}){end_bound}")
        } else {
            let wrapped_alts: Vec<String> = alts.iter().zip(alt_bounds.iter()).map(|(alt, &(start_word, end_word))| {
                let start_bound = if start_word { r"\b" } else { r"\B" };
                let end_bound = if end_word { r"\b" } else { r"\B" };
                format!(r"{start_bound}(?:{alt}){end_bound}")
            }).collect();
            wrapped_alts.join("|")
        };
        
        (pat, Some(wrapped))
    } else {
        (pat, None)
    }
}
