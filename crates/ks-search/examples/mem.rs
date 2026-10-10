#![allow(clippy::print_stdout)]
//! Memory probe: puts N synthetic paths into the walk cache, opens a finder session on it
//! (the real cache-hit path, so the matcher holds its own columns) and idles, so an outside
//! tool can read the working set. Prints `READY` when the session has settled.
//! Usage: `mem [N]`.
use ks_search::finder::{FinderItem, FinderOptions};
use ks_search::{EntryKind, FinderSession, WalkCache};
use std::sync::Arc;

fn main() {
    let n: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1_000_000);
    // Generated in chunks so the transient input strings do not inflate the peak.
    let mut items: Vec<FinderItem> = Vec::with_capacity(n);
    let mut chunk_seed = 7;
    while items.len() < n {
        let take = (n - items.len()).min(20_000);
        items.extend(
            ks_search::testgen::synthetic_paths(take, chunk_seed)
                .into_iter()
                .map(|p| FinderItem::new(p, EntryKind::File)),
        );
        chunk_seed += 1;
    }
    let avg = items.iter().map(|i| i.rel().len()).sum::<usize>() / n.max(1);
    let mut opts = FinderOptions::new("Z:/kestrel-mem-probe");
    // The probe root does not exist: serve the cache only.
    opts.refresh_cached = false;
    let cache = Arc::new(WalkCache::new());
    cache.put(opts.cache_key(), items);
    let mut s = FinderSession::start(opts, 1, Some(cache.clone()), Arc::new(|| {}));
    s.set_query("handler");
    for _ in 0..300 {
        let st = s.tick(20);
        if !st.busy() {
            break;
        }
    }
    s.tick(20);
    println!(
        "n={n} avg_len={avg} matched={} total={}",
        s.matched_count(),
        s.total_count()
    );
    println!("READY");
    std::thread::sleep(std::time::Duration::from_secs(4));
    drop(s);
    drop(cache);
}
