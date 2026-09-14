//! Popup search. Case-insensitive; an item matches when it contains every
//! whitespace-separated word of the query. Items containing the query as one
//! contiguous phrase come first, and history order is kept within each group.

/// Prepares item text for [`rank`]. Do this once per item, not per keystroke.
pub fn normalize(text: &str) -> String {
    text.to_lowercase()
}

/// Indices of the matching `haystacks` (already [`normalize`]d), best first.
pub fn rank<S: AsRef<str>>(haystacks: &[S], query: &str) -> Vec<usize> {
    let query = normalize(query);
    let words: Vec<&str> = query.split_whitespace().collect();
    if words.is_empty() {
        return (0..haystacks.len()).collect();
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn run(items: &[&str], query: &str) -> Vec<usize> {
        let haystacks: Vec<String> = items.iter().map(|item| normalize(item)).collect();
        rank(&haystacks, query)
    }

    #[test]
    fn empty_query_keeps_everything_in_order() {
        assert_eq!(run(&["a", "b", "c"], "  "), [0, 1, 2]);
    }

    #[test]
    fn case_insensitive_substring() {
        assert_eq!(run(&["Hello World", "goodbye", "ÉCOLE"], "WORLD"), [0]);
        assert_eq!(run(&["Hello World", "goodbye", "ÉCOLE"], "école"), [2]);
    }

    #[test]
    fn phrase_matches_rank_before_word_matches() {
        let items = ["world, hello", "git push", "hello world", "say hello to the world"];
        assert_eq!(run(&items, "hello world"), [2, 0, 3]);
        assert_eq!(run(&items, "hello   world"), [2, 0, 3]);
    }
}
