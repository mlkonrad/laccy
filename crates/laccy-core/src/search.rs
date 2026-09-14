//! Popup search. Case-insensitive; an item matches when every
//! whitespace-separated word of the query matches it.
//!
//! - [`Mode::Exact`]: words match as substrings. Items containing the query as
//!   one contiguous phrase come first, and history order is kept within each group.
//! - [`Mode::Fuzzy`]: a word also matches when its letters appear in order with
//!   gaps between them, like "gco" in "git checkout". Tight matches and matches
//!   at the start of words come first, and history order breaks ties.

use std::cmp::Reverse;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Exact,
    Fuzzy,
}

impl Mode {
    /// Every mode, in the order the preferences window lists them.
    pub const ALL: [Self; 2] = [Self::Exact, Self::Fuzzy];

    /// The mode's value in GSettings.
    pub fn name(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Fuzzy => "fuzzy",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.name() == name)
    }
}

/// Prepares item text for [`rank`]. Do this once per item, not per keystroke.
pub fn normalize(text: &str) -> String {
    text.to_lowercase()
}

/// Indices of the matching `haystacks` (already [`normalize`]d), best first.
pub fn rank<S: AsRef<str>>(haystacks: &[S], query: &str, mode: Mode) -> Vec<usize> {
    let query = normalize(query);
    let words: Vec<&str> = query.split_whitespace().collect();
    if words.is_empty() {
        return (0..haystacks.len()).collect();
    }
    match mode {
        Mode::Exact => rank_exact(haystacks, &words),
        Mode::Fuzzy => rank_fuzzy(haystacks, &words),
    }
}

fn rank_exact<S: AsRef<str>>(haystacks: &[S], words: &[&str]) -> Vec<usize> {
    let phrase = words.join(" ");
    let mut phrase_matches = Vec::new();
    let mut word_matches = Vec::new();
    for (index, haystack) in haystacks.iter().enumerate() {
        let haystack = haystack.as_ref();
        if haystack.contains(&phrase) {
            phrase_matches.push(index);
        } else if words.iter().all(|word| haystack.contains(word)) {
            word_matches.push(index);
        }
    }
    phrase_matches.extend(word_matches);
    phrase_matches
}

fn rank_fuzzy<S: AsRef<str>>(haystacks: &[S], words: &[&str]) -> Vec<usize> {
    let mut matches: Vec<(usize, i32)> = haystacks
        .iter()
        .enumerate()
        .filter_map(|(index, haystack)| {
            let score = words.iter().map(|word| fuzzy_score(haystack.as_ref(), word)).sum::<Option<i32>>()?;
            Some((index, score))
        })
        .collect();
    // A stable sort, so equal scores keep history order.
    matches.sort_by_key(|&(_, score)| Reverse(score));
    matches.into_iter().map(|(index, _)| index).collect()
}

// Scores follow fzf's linear-time algorithm, so long items stay fast to search.
const SCORE_MATCH: i32 = 16;
const BONUS_BOUNDARY: i32 = 8;
const BONUS_CONSECUTIVE: i32 = 4;
const PENALTY_GAP_START: i32 = 3;
const PENALTY_GAP_EXTENSION: i32 = 1;

/// How well `word` matches `haystack` as a subsequence, or `None` if it doesn't.
fn fuzzy_score(haystack: &str, word: &str) -> Option<i32> {
    // The leftmost match ends at `end`. Scanning back from there finds the
    // shortest match that ends in the same place.
    let mut end = 0;
    for c in word.chars() {
        end += haystack[end..].find(c)? + c.len_utf8();
    }
    let mut start = end;
    for c in word.chars().rev() {
        start = haystack[..start].rfind(c)?;
    }
    let mut score = score_at(haystack, start, word);
    // That misses the word typed out in full further along, which should win.
    // `contains` first because it rejects much faster than `find`.
    if end - start > word.len() && haystack.contains(word) {
        if let Some(position) = haystack.find(word) {
            score = score.max(score_at(haystack, position, word));
        }
    }
    Some(score)
}

/// Scores the leftmost match of `word` in `haystack` that starts at byte `start`.
fn score_at(haystack: &str, start: usize, word: &str) -> i32 {
    let mut wanted = word.chars().peekable();
    let mut previous = haystack[..start].chars().next_back();
    let mut score = 0;
    let mut first = true;
    let mut consecutive = false;
    // The bonus of the first letter in the current run of consecutive matches.
    let mut run_bonus = 0;

    for c in haystack[start..].chars() {
        let Some(&next) = wanted.peek() else {
            break;
        };
        if c == next {
            wanted.next();
            let boundary = if previous.is_none_or(|p| !p.is_alphanumeric()) { BONUS_BOUNDARY } else { 0 };
            let bonus = if consecutive {
                boundary.max(run_bonus).max(BONUS_CONSECUTIVE)
            } else {
                run_bonus = boundary;
                boundary
            };
            score += SCORE_MATCH + if first { bonus * 2 } else { bonus };
            first = false;
            consecutive = true;
        } else {
            score -= if consecutive { PENALTY_GAP_START } else { PENALTY_GAP_EXTENSION };
            consecutive = false;
        }
        previous = Some(c);
    }
    score
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(items: &[&str], query: &str, mode: Mode) -> Vec<usize> {
        let haystacks: Vec<String> = items.iter().map(|item| normalize(item)).collect();
        rank(&haystacks, query, mode)
    }

    #[test]
    fn empty_query_keeps_everything_in_order() {
        for mode in Mode::ALL {
            assert_eq!(run(&["a", "b", "c"], "  ", mode), [0, 1, 2]);
        }
    }

    #[test]
    fn case_insensitive_substring() {
        for mode in Mode::ALL {
            assert_eq!(run(&["Hello World", "goodbye", "ÉCOLE"], "WORLD", mode), [0]);
            assert_eq!(run(&["Hello World", "goodbye", "ÉCOLE"], "école", mode), [2]);
        }
    }

    #[test]
    fn phrase_matches_rank_before_word_matches() {
        let items = ["world, hello", "git push", "hello world", "say hello to the world"];
        assert_eq!(run(&items, "hello world", Mode::Exact), [2, 0, 3]);
        assert_eq!(run(&items, "hello   world", Mode::Exact), [2, 0, 3]);
    }

    #[test]
    fn exact_mode_needs_contiguous_letters() {
        assert!(run(&["git checkout"], "gco", Mode::Exact).is_empty());
    }

    #[test]
    fn fuzzy_matches_letters_in_order() {
        let items = ["git checkout main", "go home", "checkout", "ÉCOLE"];
        assert_eq!(run(&items, "gco", Mode::Fuzzy), [0]);
        assert_eq!(run(&items, "ma gco", Mode::Fuzzy), [0]);
        assert_eq!(run(&items, "ocg", Mode::Fuzzy), [] as [usize; 0]);
        assert_eq!(run(&items, "écl", Mode::Fuzzy), [3]);
    }

    #[test]
    fn fuzzy_prefers_tight_matches_at_word_starts() {
        assert_eq!(run(&["magic", "git commit", "gc"], "gc", Mode::Fuzzy), [2, 1, 0]);
    }

    #[test]
    fn fuzzy_finds_the_best_match_after_a_scattered_one() {
        assert_eq!(run(&["axxxb", "axxxb ab"], "ab", Mode::Fuzzy), [1, 0]);
    }

    #[test]
    fn fuzzy_ties_keep_history_order() {
        assert_eq!(run(&["one test", "two test", "three test"], "test", Mode::Fuzzy), [0, 1, 2]);
    }

    #[test]
    fn mode_names_round_trip() {
        for mode in Mode::ALL {
            assert_eq!(Mode::from_name(mode.name()), Some(mode));
        }
        assert_eq!(Mode::from_name("regex"), None);
    }
}
