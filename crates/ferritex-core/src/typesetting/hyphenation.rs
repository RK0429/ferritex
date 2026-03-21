use std::collections::BTreeSet;

pub trait Hyphenator {
    fn hyphenate(&self, word: &str) -> Vec<usize>;
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SimpleHyphenator;

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
        let break_index = if cluster_len == 1 {
            cluster_start
        } else if cluster_len == 2 && is_digraph(chars[cluster_start], chars[cluster_start + 1]) {
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

#[cfg(test)]
mod tests {
    use super::{Hyphenator, SimpleHyphenator};

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
}
