//! Popup search. Case-insensitive in every mode.
//!
//! - [`Mode::Exact`]: every whitespace-separated word of the query matches as a
//!   substring. Items containing the query as one contiguous phrase come first,
//!   and history order is kept within each group.
//! - [`Mode::Fuzzy`]: like exact, but a word also matches when its letters appear
//!   in order with gaps between them, like "gco" in "git checkout". Tight matches
//!   and matches at the start of words come first, and history order breaks ties.
//! - [`Mode::Regex`]: the whole query is one regular expression (GLib's, which is
//!   PCRE2), where `^` and `$` also match at line breaks. History order is kept.
//! - [`Mode::Mixed`]: as in Maccy, exact, then regex, then fuzzy, keeping the
//!   results of the first that finds anything. An invalid pattern finds nothing.

use std::cmp::Reverse;
use std::ops::Range;

use gio::glib;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Exact,
    Fuzzy,
    Regex,
    Mixed,
}

impl Mode {
    /// Every mode, in the order the preferences window lists them.
    pub const ALL: [Self; 4] = [Self::Exact, Self::Fuzzy, Self::Regex, Self::Mixed];

    /// The mode's value in GSettings.
    pub fn name(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Fuzzy => "fuzzy",
            Self::Regex => "regex",
            Self::Mixed => "mixed",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.name() == name)
    }
}

/// Prepares item text for [`rank`] in `mode`. Do this once per item, not per keystroke.
pub fn normalize(text: &str, mode: Mode) -> String {
    match mode {
        // Mixed searches lowercased text with patterns too. They ignore case, so
        // that only changes classes like \p{Lu} (an uppercase letter).
        Mode::Exact | Mode::Fuzzy | Mode::Mixed => text.to_lowercase(),
        // Patterns ignore case themselves, and lowercasing the query would turn
        // escapes like \D (not a digit) into others (\d, a digit).
        Mode::Regex => text.to_owned(),
    }
}

/// Indices of the matching `haystacks` (already [`normalize`]d for `mode`), best
/// first, and the mode that matched them, which is `mode` unless it's
/// [`Mode::Mixed`]. Fails only in [`Mode::Regex`], when `query` isn't a valid pattern.
pub fn rank<S: AsRef<str>>(haystacks: &[S], query: &str, mode: Mode) -> Result<(Vec<usize>, Mode), glib::Error> {
    let normalized = normalize(query, mode);
    let words: Vec<&str> = normalized.split_whitespace().collect();
    if words.is_empty() {
        return Ok(((0..haystacks.len()).collect(), mode));
    }
    match mode {
        Mode::Exact => Ok((rank_exact(haystacks, &words), mode)),
        Mode::Fuzzy => Ok((rank_fuzzy(haystacks, &words), mode)),
        Mode::Regex => Ok((rank_regex(haystacks, &normalized)?, mode)),
        Mode::Mixed => {
            let exact = rank_exact(haystacks, &words);
            if !exact.is_empty() {
                return Ok((exact, Mode::Exact));
            }
            let regex = rank_regex(haystacks, query).unwrap_or_default();
            if !regex.is_empty() {
                return Ok((regex, Mode::Regex));
            }
            Ok((rank_fuzzy(haystacks, &words), Mode::Fuzzy))
        }
    }
}

/// The byte ranges of `text` that `query` matches in `mode`, sorted and merged,
/// for highlighting, where `mode` is the one [`rank`] says matched. `text` is what a row shows rather than what [`rank`]
/// searched, so a matching item can have nothing to highlight.
pub fn highlights(text: &str, query: &str, mode: Mode) -> Vec<Range<usize>> {
    if query.trim().is_empty() {
        return Vec::new();
    }
    let mut ranges = match mode {
        Mode::Exact | Mode::Fuzzy | Mode::Mixed => {
            // Lowercased a character at a time, so each byte maps back to its character in `text`.
            let mut lower = String::new();
            let mut origins = Vec::new();
            for (index, c) in text.char_indices() {
                for lower_c in c.to_lowercase() {
                    lower.push(lower_c);
                    origins.resize(lower.len(), index);
                }
            }
            let query: String = query.chars().flat_map(char::to_lowercase).collect();
            let words: Vec<&str> = query.split_whitespace().collect();
            // The whole phrase too, so the spaces between its words are marked with them.
            let phrase = words.join(" ");
            let mut ranges: Vec<Range<usize>> =
                lower.match_indices(&phrase).map(|(start, phrase)| start..start + phrase.len()).collect();
            for &word in &words {
                let before = ranges.len();
                ranges.extend(lower.match_indices(word).map(|(start, word)| start..start + word.len()));
                if ranges.len() == before && mode == Mode::Fuzzy {
                    ranges.extend(fuzzy_positions(&lower, word));
                }
            }
            ranges
                .into_iter()
                .map(|range| {
                    let last = origins[range.end - 1];
                    origins[range.start]..last + text[last..].chars().next().map_or(0, char::len_utf8)
                })
                .collect()
        }
        Mode::Regex => regex_ranges(text, query),
    };
    ranges.sort_by_key(|range| range.start);
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
    for range in ranges {
        match merged.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => merged.push(range),
        }
    }
    merged
}

/// Every non-empty match of `pattern` in `text`, matched like [`rank_regex`].
fn regex_ranges(text: &str, pattern: &str) -> Vec<Range<usize>> {
    use glib::{IntoGStr, RegexCompileFlags as Compile, RegexMatchFlags as Match};

    let Ok(Some(regex)) = glib::Regex::new(pattern, Compile::CASELESS | Compile::MULTILINE, Match::DEFAULT) else {
        return Vec::new();
    };
    text.run_with_gstr(|text| {
        let mut ranges = Vec::new();
        let Ok(info) = regex.match_(text, Match::DEFAULT) else {
            return ranges;
        };
        while info.matches() {
            if let Some((start, end)) = info.fetch_pos(0) {
                if end > start {
                    ranges.push(start as usize..end as usize);
                }
            }
            if info.next().is_err() {
                break;
            }
        }
        ranges
    })
}

fn rank_regex<S: AsRef<str>>(haystacks: &[S], pattern: &str) -> Result<Vec<usize>, glib::Error> {
    use glib::{IntoGStr, RegexCompileFlags as Compile, RegexMatchFlags as Match};

    let flags = Compile::CASELESS | Compile::MULTILINE | Compile::OPTIMIZE;
    let Some(regex) = glib::Regex::new(pattern, flags, Match::DEFAULT)? else {
        return Ok(Vec::new());
    };
    Ok(haystacks
        .iter()
        .enumerate()
        .filter(|(_, haystack)| {
            let haystack = haystack.as_ref();
            // Images have empty haystacks. Patterns like ".*" match empty text, but
            // images only show without a query, as in the other modes. A match
            // that fails, like one hitting PCRE2's backtracking limit, is no match.
            !haystack.is_empty()
                && haystack.run_with_gstr(|haystack| {
                    regex.match_(haystack, Match::DEFAULT).is_ok_and(|info| info.matches())
                })
        })
        .map(|(index, _)| index)
        .collect())
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

/// The byte range of the shortest subsequence match of `word` in `haystack`
/// that ends where the leftmost one does, or `None` if there is none.
fn fuzzy_span(haystack: &str, word: &str) -> Option<Range<usize>> {
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
    Some(start..end)
}

/// The byte ranges of the letters of `word` that [`fuzzy_span`] matched.
fn fuzzy_positions(haystack: &str, word: &str) -> Vec<Range<usize>> {
    let Some(span) = fuzzy_span(haystack, word) else {
        return Vec::new();
    };
    let mut wanted = word.chars().peekable();
    let mut ranges = Vec::new();
    for (index, c) in haystack[span.clone()].char_indices() {
        if wanted.next_if_eq(&c).is_some() {
            let start = span.start + index;
            ranges.push(start..start + c.len_utf8());
        }
    }
    ranges
}

/// How well `word` matches `haystack` as a subsequence, or `None` if it doesn't.
fn fuzzy_score(haystack: &str, word: &str) -> Option<i32> {
    let Range { start, end } = fuzzy_span(haystack, word)?;
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
        let haystacks: Vec<String> = items.iter().map(|item| normalize(item, mode)).collect();
        rank(&haystacks, query, mode).unwrap().0
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
    fn regex_matches_patterns_in_history_order() {
        let items = ["see https://example.com", "http://a.b", "no link", "first line\nHTTPS://x"];
        assert_eq!(run(&items, "https?://", Mode::Regex), [0, 1, 3]);
        assert_eq!(run(&items, "^https?://", Mode::Regex), [1, 3]);
        assert_eq!(run(&["2026-09-15 notes", "notes 2026"], r"^\d{4}-\d{2}", Mode::Regex), [0]);
    }

    #[test]
    fn regex_keeps_the_query_as_written() {
        assert_eq!(run(&["123", "abc"], r"^\D+$", Mode::Regex), [1]);
        assert_eq!(run(&["a b", "ab"], "a b", Mode::Regex), [0]);
    }

    #[test]
    fn regex_never_matches_empty_haystacks() {
        assert_eq!(run(&["", "text"], ".*", Mode::Regex), [1]);
    }

    #[test]
    fn invalid_regex_is_an_error() {
        let haystacks = [normalize("f(x)", Mode::Regex)];
        assert!(rank(&haystacks, "f(", Mode::Regex).is_err());
        assert_eq!(rank(&haystacks, r"f\(", Mode::Regex).unwrap().0, [0]);
        assert_eq!(rank(&haystacks, "f(", Mode::Exact).unwrap().0, [0]);
    }

    fn mixed(items: &[&str], query: &str) -> (Vec<usize>, Mode) {
        let haystacks: Vec<String> = items.iter().map(|item| normalize(item, Mode::Mixed)).collect();
        rank(&haystacks, query, Mode::Mixed).unwrap()
    }

    #[test]
    fn mixed_prefers_exact_then_regex_then_fuzzy() {
        let items = ["git checkout", "Make -O2", "https://a.b"];
        assert_eq!(mixed(&items, "MAKE"), (vec![1], Mode::Exact));
        assert_eq!(mixed(&items, "^https?://"), (vec![2], Mode::Regex));
        assert_eq!(mixed(&items, r"-o\d"), (vec![1], Mode::Regex));
        assert_eq!(mixed(&items, "gco"), (vec![0], Mode::Fuzzy));
        assert_eq!(mixed(&items, "xyz"), (vec![], Mode::Fuzzy));
    }

    #[test]
    fn mixed_skips_invalid_patterns() {
        assert_eq!(mixed(&["f(x)"], "f("), (vec![0], Mode::Exact));
        assert_eq!(mixed(&["f(x)"], "fx)"), (vec![0], Mode::Fuzzy));
    }

    fn highlighted<'a>(text: &'a str, query: &str, mode: Mode) -> Vec<&'a str> {
        highlights(text, query, mode).into_iter().map(|range| &text[range]).collect()
    }

    #[test]
    fn exact_highlights_every_word_ignoring_case() {
        assert_eq!(highlighted("Hello World, hello", "hello world", Mode::Exact), ["Hello World", "hello"]);
        assert_eq!(highlighted("ÉCOLE école", "éc", Mode::Exact), ["ÉC", "éc"]);
        // "İ" lowercases to two characters; the highlight still covers whole characters.
        assert_eq!(highlighted("xİy", "i", Mode::Exact), ["İ"]);
        assert!(highlighted("git checkout", "gco", Mode::Exact).is_empty());
        assert!(highlighted("anything", "  ", Mode::Exact).is_empty());
    }

    #[test]
    fn fuzzy_highlights_matched_letters() {
        assert_eq!(highlighted("git checkout", "gco", Mode::Fuzzy), ["g", "c", "o"]);
        // A word typed out in full is highlighted as a whole.
        assert_eq!(highlighted("git checkout", "check", Mode::Fuzzy), ["check"]);
        assert_eq!(highlighted("··a⏎b", "ab", Mode::Fuzzy), ["a", "b"]);
    }

    #[test]
    fn regex_highlights_matches() {
        assert_eq!(highlighted("see https://a.b and http://c", "https?://", Mode::Regex), ["https://", "http://"]);
        assert_eq!(highlighted("A1b22", r"\d+", Mode::Regex), ["1", "22"]);
        assert!(highlighted("abc", "x*", Mode::Regex).is_empty());
        assert!(highlighted("f(x)", "f(", Mode::Regex).is_empty());
    }

    #[test]
    fn mode_names_round_trip() {
        for mode in Mode::ALL {
            assert_eq!(Mode::from_name(mode.name()), Some(mode));
        }
        assert_eq!(Mode::from_name("sloppy"), None);
    }
}
