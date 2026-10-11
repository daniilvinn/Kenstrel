//! In-directory filter (`/`): fuzzy-matches a query against an in-memory
//! list of names on every keystroke. Synchronous and cheap (a few thousand names take well
//! under a millisecond); no threads are involved.

use nucleo::pattern::{CaseMatching, Normalization, Pattern};
use nucleo::{Config, Matcher, Utf32Str};

/// One matching item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterMatch {
    /// Index into the input list.
    pub index: usize,
    /// Match score (higher is better).
    pub score: u32,
    /// Sorted, deduplicated **character** (not byte) indices of matched characters, for
    /// highlighting. Empty for an empty query.
    pub indices: Vec<u32>,
}

/// Reusable filter holding a matcher (avoids re-allocating scratch memory per keystroke).
pub struct ListFilter {
    matcher: Matcher,
}

impl Default for ListFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl ListFilter {
    /// New filter using default (non-path) matching, smart case.
    pub fn new() -> Self {
        Self {
            matcher: Matcher::new(Config::DEFAULT),
        }
    }

    /// Filters `items` by `query` (nucleo pattern syntax: space-separated atoms, `^`/`$`
    /// anchors, `!` negation, `'` exact). Results are sorted by descending score; ties keep
    /// the original order (stable). An empty query returns all items in original order.
    pub fn filter<S: AsRef<str>>(&mut self, items: &[S], query: &str) -> Vec<FilterMatch> {
        let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
        if pattern.atoms.is_empty() {
            return (0..items.len())
                .map(|index| FilterMatch {
                    index,
                    score: 0,
                    indices: Vec::new(),
                })
                .collect();
        }
        let mut buf = Vec::new();
        let mut out = Vec::new();
        for (index, item) in items.iter().enumerate() {
            let hay = Utf32Str::new(item.as_ref(), &mut buf);
            let mut indices = Vec::new();
            if let Some(score) = pattern.indices(hay, &mut self.matcher, &mut indices) {
                indices.sort_unstable();
                indices.dedup();
                out.push(FilterMatch {
                    index,
                    score,
                    indices,
                });
            }
        }
        // `sort_by` is stable, so equal scores keep input order.
        out.sort_by_key(|m| std::cmp::Reverse(m.score));
        out
    }
}

/// One-shot convenience wrapper around [`ListFilter::filter`].
pub fn filter_list<S: AsRef<str>>(items: &[S], query: &str) -> Vec<FilterMatch> {
    ListFilter::new().filter(items, query)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_returns_all_in_order() {
        let r = filter_list(&["b", "a", "c"], "");
        assert_eq!(r.iter().map(|m| m.index).collect::<Vec<_>>(), vec![0, 1, 2]);
        assert!(r.iter().all(|m| m.indices.is_empty()));
    }

    #[test]
    fn indices_point_at_matched_chars() {
        let items = ["Cargo.toml", "README.md", "main.rs"];
        let r = filter_list(&items, "rdm");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].index, 1);
        let chars: Vec<char> = items[1].chars().collect();
        let matched: String = r[0].indices.iter().map(|&i| chars[i as usize]).collect();
        assert_eq!(matched.to_lowercase(), "rdm");
    }

    #[test]
    fn sorted_by_score_and_stable() {
        let items = ["xxabxx", "ab", "ab", "a_b"];
        let r = filter_list(&items, "ab");
        assert_eq!(r[0].score, r[1].score);
        assert_eq!((r[0].index, r[1].index), (1, 2), "ties keep input order");
        assert!(r.windows(2).all(|w| w[0].score >= w[1].score));
        assert_eq!(r.len(), 4);
    }

    #[test]
    fn smart_case() {
        let items = ["readme", "README"];
        assert_eq!(filter_list(&items, "readme").len(), 2);
        let r = filter_list(&items, "README");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].index, 1);
    }

    #[test]
    fn unicode_char_indices() {
        let items = ["привет.txt"];
        let r = filter_list(&items, "вет");
        assert_eq!(r[0].indices, vec![3, 4, 5]);
    }

    #[test]
    fn no_match() {
        assert!(filter_list(&["abc"], "zzz").is_empty());
    }
}
