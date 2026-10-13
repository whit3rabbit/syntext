//! Porter stemmer (M. F. Porter, 1980), a line-by-line port of the reference
//! C implementation including its two documented departures from the paper
//! (`bli -> ble` and the extra `logi -> log` rule).
//!
//! Operates on lowercase ASCII letters only; callers pass anything else
//! through unchanged. Stemming collapses `validates`, `validated`,
//! `validation`, and `validating` onto one term (`valid`), which is what lets
//! a question phrased one way find text phrased another way without an
//! embedding model.
//!
//! Indices are `isize` because the reference algorithm lets the stem end `j`
//! become -1 when a whole word is a suffix; every reader of `j` handles that
//! case the way the C code does (`m() == 0`, no vowel in the stem).

/// Stem a lowercase ASCII word. Words of two letters or fewer, and words
/// containing anything other than `a..=z`, are returned unchanged.
pub(crate) fn porter_stem(word: &str) -> String {
    if word.len() <= 2 || !word.bytes().all(|b| b.is_ascii_lowercase()) {
        return word.to_string();
    }
    let mut z = Stemmer {
        b: word.as_bytes().to_vec(),
        k: word.len() as isize - 1,
        j: 0,
    };
    z.step1ab();
    if z.k > 0 {
        z.step1c();
        z.step2();
        z.step3();
        z.step4();
        z.step5();
    }
    z.b.truncate((z.k + 1) as usize);
    // Only ASCII letters were ever written, so this cannot fail.
    String::from_utf8(z.b).unwrap_or_else(|_| word.to_string())
}

struct Stemmer {
    b: Vec<u8>,
    /// Index of the last byte of the current word.
    k: isize,
    /// Index of the last byte of the stem, set by a successful `ends`.
    j: isize,
}

impl Stemmer {
    fn at(&self, i: isize) -> u8 {
        self.b[i as usize]
    }

    /// True if `b[i]` is a consonant.
    fn cons(&self, i: isize) -> bool {
        match self.at(i) {
            b'a' | b'e' | b'i' | b'o' | b'u' => false,
            b'y' => i == 0 || !self.cons(i - 1),
            _ => true,
        }
    }

    /// Number of consonant-vowel sequences in `b[0..=j]` (Porter's m).
    fn m(&self) -> usize {
        let j = self.j;
        let mut n = 0;
        let mut i = 0;
        loop {
            if i > j {
                return n;
            }
            if !self.cons(i) {
                break;
            }
            i += 1;
        }
        i += 1;
        loop {
            loop {
                if i > j {
                    return n;
                }
                if self.cons(i) {
                    break;
                }
                i += 1;
            }
            i += 1;
            n += 1;
            loop {
                if i > j {
                    return n;
                }
                if !self.cons(i) {
                    break;
                }
                i += 1;
            }
            i += 1;
        }
    }

    /// True if `b[0..=j]` contains a vowel.
    fn vowel_in_stem(&self) -> bool {
        (0..=self.j).any(|i| !self.cons(i))
    }

    /// True if `b[j-1..=j]` is a double consonant.
    fn double_c(&self, j: isize) -> bool {
        j >= 1 && self.at(j) == self.at(j - 1) && self.cons(j)
    }

    /// True if `b[i-2..=i]` is consonant-vowel-consonant and the final
    /// consonant is not w, x, or y (e.g. `hop`, but not `snow` or `box`).
    fn cvc(&self, i: isize) -> bool {
        if i < 2 || !self.cons(i) || self.cons(i - 1) || !self.cons(i - 2) {
            return false;
        }
        !matches!(self.at(i), b'w' | b'x' | b'y')
    }

    /// True if `b[0..=k]` ends with `s`; on success sets `j` to the end of
    /// the stem that precedes the suffix.
    fn ends(&mut self, s: &str) -> bool {
        let len = s.len() as isize;
        if len > self.k + 1 {
            return false;
        }
        let start = (self.k + 1 - len) as usize;
        if &self.b[start..=self.k as usize] != s.as_bytes() {
            return false;
        }
        self.j = self.k - len;
        true
    }

    /// Replace `b[j+1..=k]` with `s`.
    fn set_to(&mut self, s: &str) {
        self.b.truncate((self.j + 1) as usize);
        self.b.extend_from_slice(s.as_bytes());
        self.k = self.j + s.len() as isize;
    }

    /// `set_to(s)` when m() > 0.
    fn r(&mut self, s: &str) {
        if self.m() > 0 {
            self.set_to(s);
        }
    }

    /// Plurals and -ed / -ing.
    fn step1ab(&mut self) {
        if self.at(self.k) == b's' {
            if self.ends("sses") {
                self.k -= 2;
            } else if self.ends("ies") {
                self.set_to("i");
            } else if self.at(self.k - 1) != b's' {
                self.k -= 1;
            }
        }
        if self.ends("eed") {
            if self.m() > 0 {
                self.k -= 1;
            }
        } else if (self.ends("ed") || self.ends("ing")) && self.vowel_in_stem() {
            self.k = self.j;
            if self.ends("at") {
                self.set_to("ate");
            } else if self.ends("bl") {
                self.set_to("ble");
            } else if self.ends("iz") {
                self.set_to("ize");
            } else if self.double_c(self.k) {
                self.k -= 1;
                if matches!(self.at(self.k), b'l' | b's' | b'z') {
                    self.k += 1;
                }
            } else if self.m() == 1 && self.cvc(self.k) {
                // The failed `ends` calls above left `j == k`, so this
                // appends the `e`.
                self.set_to("e");
            }
        }
    }

    /// Terminal y -> i when there is another vowel in the stem.
    fn step1c(&mut self) {
        if self.ends("y") && self.vowel_in_stem() {
            let k = self.k as usize;
            self.b[k] = b'i';
        }
    }

    /// Double suffixes -> single ones (`-ization` -> `-ize`).
    fn step2(&mut self) {
        let rules: &[(&str, &str)] = match self.at(self.k - 1) {
            b'a' => &[("ational", "ate"), ("tional", "tion")],
            b'c' => &[("enci", "ence"), ("anci", "ance")],
            b'e' => &[("izer", "ize")],
            b'l' => &[
                ("bli", "ble"),
                ("alli", "al"),
                ("entli", "ent"),
                ("eli", "e"),
                ("ousli", "ous"),
            ],
            b'o' => &[("ization", "ize"), ("ation", "ate"), ("ator", "ate")],
            b's' => &[
                ("alism", "al"),
                ("iveness", "ive"),
                ("fulness", "ful"),
                ("ousness", "ous"),
            ],
            b't' => &[("aliti", "al"), ("iviti", "ive"), ("biliti", "ble")],
            b'g' => &[("logi", "log")],
            _ => &[],
        };
        self.apply_first(rules);
    }

    /// `-ic-`, `-full`, `-ness` and friends.
    fn step3(&mut self) {
        let rules: &[(&str, &str)] = match self.at(self.k) {
            b'e' => &[("icate", "ic"), ("ative", ""), ("alize", "al")],
            b'i' => &[("iciti", "ic")],
            b'l' => &[("ical", "ic"), ("ful", "")],
            b's' => &[("ness", "")],
            _ => &[],
        };
        self.apply_first(rules);
    }

    /// Apply the first rule whose suffix matches; later rules are not tried
    /// even when the first one's measure condition fails.
    fn apply_first(&mut self, rules: &[(&str, &str)]) {
        for (suffix, replacement) in rules {
            if self.ends(suffix) {
                self.r(replacement);
                return;
            }
        }
    }

    /// Strip `-ant`, `-ence` etc. in context `<c>vcvc<v>`.
    fn step4(&mut self) {
        let suffixes: &[&str] = match self.at(self.k - 1) {
            b'a' => &["al"],
            b'c' => &["ance", "ence"],
            b'e' => &["er"],
            b'i' => &["ic"],
            b'l' => &["able", "ible"],
            b'n' => &["ant", "ement", "ment", "ent"],
            b'o' => &["ion", "ou"],
            b's' => &["ism"],
            b't' => &["ate", "iti"],
            b'u' => &["ous"],
            b'v' => &["ive"],
            b'z' => &["ize"],
            _ => return,
        };
        let mut matched = false;
        for suffix in suffixes {
            if self.ends(suffix) {
                // -ion is removed only after s or t.
                if *suffix == "ion" && !(self.j >= 0 && matches!(self.at(self.j), b's' | b't')) {
                    continue;
                }
                matched = true;
                break;
            }
        }
        if matched && self.m() > 1 {
            self.k = self.j;
        }
    }

    /// Remove a final -e and reduce -ll to -l when the measure allows.
    fn step5(&mut self) {
        self.j = self.k;
        if self.at(self.k) == b'e' {
            let a = self.m();
            if a > 1 || (a == 1 && !self.cvc(self.k - 1)) {
                self.k -= 1;
            }
        }
        if self.at(self.k) == b'l' && self.double_c(self.k) && self.m() > 1 {
            self.k -= 1;
        }
    }
}

#[cfg(test)]
#[path = "stem_tests.rs"]
mod tests;
