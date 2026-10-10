#![allow(clippy::print_stdout)]
//! Memory probe: builds N synthetic paths twice (cache copy + nucleo copy) and idles,
//! so an outside tool can read the peak working set. Usage: `mem [N]`.
use ks_search::finder::{FinderItem, FinderOptions};
use ks_search::{EntryKind, FinderSession};
use std::sync::Arc;

fn main() {
    let n: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1_000_000);
    let items: Vec<FinderItem> = ks_search::testgen::synthetic_paths(n, 7)
        .into_iter()
        .map(|p| FinderItem::new(p.into_boxed_str(), EntryKind::File))
        .collect();
    let avg = items.iter().map(|i| i.rel.len()).sum::<usize>() / n.max(1);
    // The cache holds one Arc<Vec<FinderItem>>; the session holds nucleo's own copy.
    let cache = Arc::new(items.clone());
    let mut s = FinderSession::from_items(FinderOptions::new("."), 1, items, Arc::new(|| {}));
    s.set_query("handler");
    for _ in 0..50 {
        s.tick(20);
    }
    println!(
        "n={n} avg_len={avg} matched={} cache_len={}",
        s.matched_count(),
        cache.len()
    );
    std::thread::sleep(std::time::Duration::from_secs(2));
}
