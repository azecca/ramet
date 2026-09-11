//! Fuzzy matching of words, to suggest a command after a typo.
//!
//! This is the Ratcliff/Obershelp measure used by Python's
//! `difflib.get_close_matches`: similar enough to what users expect from
//! "did you mean" suggestions, and with a well-known threshold.

/// Minimum similarity, between 0 and 1, for a word to be suggested.
const CUTOFF: f64 = 0.6;

/// Similarity of two words, between 0 (nothing in common) and 1 (identical):
/// twice the number of matching characters over the total length.
pub fn similarity(a: &str, b: &str) -> f64 {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let total = a.len() + b.len();
    if total == 0 {
        return 1.0;
    }
    let matching = matching_characters(&a, &b, 0..a.len(), 0..b.len());
    // Word lengths are far below the 2^52 limit of exact float conversion.
    #[allow(clippy::cast_precision_loss)]
    let ratio = 2.0 * matching as f64 / total as f64;
    ratio
}

/// The candidate most similar to `word`, provided it reaches the cutoff.
/// Ties go to the candidate that sorts last, as in `difflib`.
pub fn closest<'a>(word: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    candidates
        .into_iter()
        .map(|candidate| (similarity(candidate, word), candidate))
        .filter(|&(score, _)| score >= CUTOFF)
        .max_by(|left, right| left.0.total_cmp(&right.0).then_with(|| left.1.cmp(right.1)))
        .map(|(_, candidate)| candidate)
}

/// Counts the characters of `a[a_range]` and `b[b_range]` covered by matching
/// blocks: the longest common block, then recursively the blocks on each side.
fn matching_characters(
    a: &[char],
    b: &[char],
    a_range: std::ops::Range<usize>,
    b_range: std::ops::Range<usize>,
) -> usize {
    let (a_start, b_start, size) = longest_match(a, b, a_range.clone(), b_range.clone());
    if size == 0 {
        return 0;
    }
    let before = matching_characters(a, b, a_range.start..a_start, b_range.start..b_start);
    let after = matching_characters(
        a,
        b,
        a_start + size..a_range.end,
        b_start + size..b_range.end,
    );
    size + before + after
}

/// The longest block common to both ranges, as `(start in a, start in b, size)`.
/// On ties, the block that starts earliest in `a`, then in `b`, wins.
fn longest_match(
    a: &[char],
    b: &[char],
    a_range: std::ops::Range<usize>,
    b_range: std::ops::Range<usize>,
) -> (usize, usize, usize) {
    let mut best = (a_range.start, b_range.start, 0);
    // `lengths[j + 1]` is the length of the common block ending at a[i - 1], b[j].
    let mut previous = vec![0; b.len() + 1];
    for i in a_range {
        let mut current = vec![0; b.len() + 1];
        for j in b_range.clone() {
            if a[i] == b[j] {
                let length = previous[j] + 1;
                current[j + 1] = length;
                if length > best.2 {
                    best = (i + 1 - length, j + 1 - length, length);
                }
            }
        }
        previous = current;
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    const KNOWN: [&str; 6] = ["checkpoint", "restore", "logs", "log", "up", "ls"];

    #[test]
    fn identical_words_are_fully_similar() {
        assert!((similarity("restore", "restore") - 1.0).abs() < f64::EPSILON);
        assert!(similarity("abc", "xyz").abs() < f64::EPSILON);
    }

    #[test]
    fn matches_python_difflib() {
        // difflib.SequenceMatcher(None, "checkpoint", "checkpint").ratio()
        assert!((similarity("checkpoint", "checkpint") - 18.0 / 19.0).abs() < 1e-12);
        // difflib.SequenceMatcher(None, "abcd", "bcda").ratio() == 0.75
        assert!((similarity("abcd", "bcda") - 0.75).abs() < 1e-12);
    }

    #[test]
    fn suggests_the_intended_command() {
        assert_eq!(closest("checkpint", KNOWN), Some("checkpoint"));
        assert_eq!(closest("restor", KNOWN), Some("restore"));
        assert_eq!(closest("logss", KNOWN), Some("logs"));
    }

    #[test]
    fn suggests_nothing_for_an_unrelated_word() {
        assert_eq!(closest("zzzqqq", KNOWN), None);
    }
}
