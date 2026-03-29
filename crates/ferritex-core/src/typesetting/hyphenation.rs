use super::hyph_en_us;
use std::{
    collections::{BTreeSet, HashMap},
    sync::{Arc, OnceLock},
};

pub trait Hyphenator {
    fn hyphenate(&self, word: &str) -> Vec<usize>;
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SimpleHyphenator;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TexPatternHyphenator {
    patterns: Arc<HashMap<String, Vec<u8>>>,
    exceptions: Arc<HashMap<String, Vec<usize>>>,
    left_hyphen_min: usize,
    right_hyphen_min: usize,
}

const PREFIXES: &[&str] = &["un", "re", "pre", "dis", "mis"];
const SUFFIXES: &[&str] = &[
    "ation", "ition", "tion", "sion", "ment", "ness", "able", "ible", "ing", "ly",
];

impl Hyphenator for SimpleHyphenator {
    fn hyphenate(&self, word: &str) -> Vec<usize> {
        let chars = word.chars().collect::<Vec<_>>();
        if chars.len() < 5 || !chars.iter().all(|ch| ch.is_ascii_alphabetic()) {
            return Vec::new();
        }

        let normalized = chars
            .iter()
            .map(|ch| ch.to_ascii_lowercase())
            .collect::<Vec<_>>();
        let mut breaks = BTreeSet::new();

        collect_prefix_breaks(&normalized, &mut breaks);
        collect_suffix_breaks(&normalized, &mut breaks);
        collect_cluster_breaks(&normalized, &mut breaks);

        let byte_offsets = byte_offsets(word);
        breaks
            .into_iter()
            .map(|index| byte_offsets[index])
            .collect()
    }
}

impl TexPatternHyphenator {
    pub fn new(patterns: &str, exceptions: &str) -> Self {
        Self::with_hyphen_mins(patterns, exceptions, 2, 3)
    }

    pub fn english() -> Self {
        static ENGLISH: OnceLock<TexPatternHyphenator> = OnceLock::new();

        ENGLISH
            .get_or_init(|| Self::new(hyph_en_us::PATTERNS, hyph_en_us::EXCEPTIONS))
            .clone()
    }

    fn with_hyphen_mins(
        patterns: &str,
        exceptions: &str,
        left_hyphen_min: usize,
        right_hyphen_min: usize,
    ) -> Self {
        Self {
            patterns: Arc::new(parse_patterns(patterns)),
            exceptions: Arc::new(parse_exceptions(exceptions)),
            left_hyphen_min,
            right_hyphen_min,
        }
    }
}

impl Hyphenator for TexPatternHyphenator {
    fn hyphenate(&self, word: &str) -> Vec<usize> {
        let chars = word.chars().collect::<Vec<_>>();
        if chars.len() < self.left_hyphen_min + self.right_hyphen_min
            || !chars.iter().all(|ch| ch.is_ascii_alphabetic())
        {
            return Vec::new();
        }

        let lower = chars
            .iter()
            .map(|ch| ch.to_ascii_lowercase())
            .collect::<String>();
        let byte_offsets = byte_offsets(word);

        if let Some(exception_breaks) = self.exceptions.get(&lower) {
            return exception_breaks
                .iter()
                .map(|&index| byte_offsets[index])
                .collect();
        }

        let wrapped = format!(".{lower}.");
        let mut scores = vec![0; wrapped.len() + 1];

        for start in 0..wrapped.len() {
            for end in start + 1..=wrapped.len() {
                let Some(pattern_values) = self.patterns.get(&wrapped[start..end]) else {
                    continue;
                };
                merge_pattern_values(
                    &mut scores[start..start + pattern_values.len()],
                    pattern_values,
                );
            }
        }

        let last_break = chars.len() - self.right_hyphen_min;
        (self.left_hyphen_min..=last_break)
            .filter(|&index| scores[index + 1] % 2 == 1)
            .map(|index| byte_offsets[index])
            .collect()
    }
}

fn collect_prefix_breaks(chars: &[char], breaks: &mut BTreeSet<usize>) {
    let word = chars.iter().collect::<String>();

    for prefix in PREFIXES {
        let prefix_len = prefix.chars().count();
        if word.starts_with(prefix) {
            insert_break(chars.len(), prefix_len, breaks);
        }
    }
}

fn collect_suffix_breaks(chars: &[char], breaks: &mut BTreeSet<usize>) {
    let word = chars.iter().collect::<String>();

    for suffix in SUFFIXES {
        if !word.ends_with(suffix) {
            continue;
        }

        let suffix_len = suffix.chars().count();
        let stem_len = chars.len().saturating_sub(suffix_len);
        if stem_len < 2 {
            continue;
        }

        let break_index = match *suffix {
            "ing" if has_double_consonant(chars, stem_len) => stem_len - 1,
            _ => stem_len,
        };
        insert_break(chars.len(), break_index, breaks);
    }
}

fn collect_cluster_breaks(chars: &[char], breaks: &mut BTreeSet<usize>) {
    for left in 0..chars.len().saturating_sub(2) {
        if !is_vowel(chars[left]) || is_vowel(chars[left + 1]) {
            continue;
        }

        let cluster_start = left + 1;
        let mut cluster_end = cluster_start;
        while cluster_end < chars.len() && !is_vowel(chars[cluster_end]) {
            cluster_end += 1;
        }

        if cluster_end >= chars.len() {
            continue;
        }

        let cluster_len = cluster_end - cluster_start;
        let break_index = if cluster_len == 1
            || (cluster_len == 2 && is_digraph(chars[cluster_start], chars[cluster_start + 1]))
        {
            cluster_start
        } else {
            cluster_end - 1
        };

        insert_break(chars.len(), break_index, breaks);
    }
}

fn insert_break(word_len: usize, break_index: usize, breaks: &mut BTreeSet<usize>) {
    if (2..=word_len.saturating_sub(2)).contains(&break_index) {
        breaks.insert(break_index);
    }
}

fn has_double_consonant(chars: &[char], stem_len: usize) -> bool {
    stem_len >= 2 && chars[stem_len - 1] == chars[stem_len - 2] && !is_vowel(chars[stem_len - 1])
}

fn is_vowel(ch: char) -> bool {
    matches!(ch, 'a' | 'e' | 'i' | 'o' | 'u' | 'y')
}

fn is_digraph(first: char, second: char) -> bool {
    matches!(
        (first, second),
        ('c', 'h')
            | ('c', 'k')
            | ('g', 'h')
            | ('p', 'h')
            | ('q', 'u')
            | ('s', 'h')
            | ('t', 'h')
            | ('w', 'h')
    )
}

fn byte_offsets(word: &str) -> Vec<usize> {
    let mut offsets = Vec::with_capacity(word.chars().count() + 1);
    let mut total = 0;
    offsets.push(total);
    for ch in word.chars() {
        total += ch.len_utf8();
        offsets.push(total);
    }
    offsets
}

fn parse_patterns(patterns: &str) -> HashMap<String, Vec<u8>> {
    patterns
        .lines()
        .map(str::trim)
        .filter(|pattern| !pattern.is_empty())
        .map(parse_pattern)
        .collect()
}

fn parse_pattern(pattern: &str) -> (String, Vec<u8>) {
    let mut letters = String::with_capacity(pattern.len());
    let mut values = vec![0];

    for ch in pattern.chars() {
        if let Some(digit) = ch.to_digit(10) {
            let slot = values
                .last_mut()
                .expect("pattern values always contain an initial slot");
            *slot = digit as u8;
        } else {
            letters.push(ch);
            values.push(0);
        }
    }

    (letters, values)
}

fn parse_exceptions(exceptions: &str) -> HashMap<String, Vec<usize>> {
    exceptions
        .lines()
        .map(str::trim)
        .filter(|exception| !exception.is_empty())
        .map(|exception| {
            let mut word = String::with_capacity(exception.len());
            let mut breakpoints = Vec::new();
            let mut char_count = 0;

            for ch in exception.chars() {
                if ch == '-' {
                    breakpoints.push(char_count);
                } else {
                    word.push(ch);
                    char_count += 1;
                }
            }

            (word, breakpoints)
        })
        .collect()
}

fn merge_pattern_values(target: &mut [u8], pattern_values: &[u8]) {
    for (slot, value) in target.iter_mut().zip(pattern_values) {
        *slot = (*slot).max(*value);
    }
}

#[cfg(test)]
mod tests {
    use super::{Hyphenator, SimpleHyphenator, TexPatternHyphenator};

    #[test]
    fn short_words_are_not_hyphenated() {
        assert_eq!(SimpleHyphenator.hyphenate("word"), Vec::<usize>::new());
    }

    #[test]
    fn skips_non_alphabetic_words() {
        assert_eq!(SimpleHyphenator.hyphenate("word2"), Vec::<usize>::new());
        assert_eq!(SimpleHyphenator.hyphenate("co-op"), Vec::<usize>::new());
    }

    #[test]
    fn finds_prefix_breaks() {
        assert_eq!(SimpleHyphenator.hyphenate("replay"), vec![2, 3]);
    }

    #[test]
    fn finds_suffix_breaks() {
        assert_eq!(SimpleHyphenator.hyphenate("mention"), vec![3]);
    }

    #[test]
    fn finds_cluster_breaks() {
        assert_eq!(SimpleHyphenator.hyphenate("basket"), vec![3]);
    }

    #[test]
    fn keeps_common_digraphs_together() {
        assert_eq!(SimpleHyphenator.hyphenate("teacher"), vec![3]);
    }

    #[test]
    fn combines_multiple_rules_deterministically() {
        assert_eq!(SimpleHyphenator.hyphenate("hyphenation"), vec![2, 5, 6, 7]);
    }

    #[test]
    fn prefers_double_consonant_break_before_ing() {
        assert_eq!(SimpleHyphenator.hyphenate("running"), vec![3]);
    }

    #[test]
    fn tex_pattern_hyphenator_matches_known_tex_breakpoints() {
        let hyphenator = TexPatternHyphenator::english();

        assert_eq!(hyphenator.hyphenate("hyphenation"), vec![2, 6]);
        assert_eq!(hyphenator.hyphenate("algorithm"), vec![2, 4]);
        assert_eq!(hyphenator.hyphenate("computer"), vec![3]);
        assert_eq!(hyphenator.hyphenate("university"), vec![3, 6]);
        assert_eq!(hyphenator.hyphenate("basket"), vec![3]);
    }

    #[test]
    fn tex_pattern_hyphenator_skips_short_words() {
        let hyphenator = TexPatternHyphenator::english();

        assert_eq!(hyphenator.hyphenate("ship"), Vec::<usize>::new());
    }

    #[test]
    fn tex_pattern_hyphenator_enforces_left_and_right_minimums() {
        let hyphenator = TexPatternHyphenator::new("a1b\nab1c\nabc1d\ncd1e\n", "");

        assert_eq!(hyphenator.hyphenate("abcde"), vec![2]);
    }

    #[test]
    fn tex_pattern_hyphenator_uses_exceptions_over_patterns() {
        let hyphenator = TexPatternHyphenator::english();
        let without_exceptions =
            TexPatternHyphenator::new(crate::typesetting::hyph_en_us::PATTERNS, "");

        assert_eq!(without_exceptions.hyphenate("project"), vec![3]);
        assert_eq!(hyphenator.hyphenate("project"), Vec::<usize>::new());
        assert_eq!(without_exceptions.hyphenate("table"), Vec::<usize>::new());
        assert_eq!(hyphenator.hyphenate("table"), vec![2]);
    }

    #[test]
    fn tex_pattern_hyphenator_skips_non_alphabetic_words() {
        let hyphenator = TexPatternHyphenator::english();

        assert_eq!(hyphenator.hyphenate("word2"), Vec::<usize>::new());
        assert_eq!(hyphenator.hyphenate("co-op"), Vec::<usize>::new());
    }
}
