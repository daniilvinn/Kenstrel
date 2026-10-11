//! Frecency database for the `z` jump, modelled on zoxide.
//!
//! Every visit to a directory adds 1 to its *rank* and records the time. The score used for
//! ordering multiplies the rank by a factor that depends on how long ago the last visit was
//! (zoxide's buckets): within the hour x4, within the day x2, within the week x0.5, older
//! x0.25. To keep the database bounded, whenever the ranks add up to more than
//! [`MAX_TOTAL_RANK`] every rank is aged (multiplied by 0.9) and entries below 1 are dropped;
//! the number of entries is capped too.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Sum of all ranks above which the database is aged (zoxide's default `_ZO_MAXAGE`).
pub const MAX_TOTAL_RANK: f64 = 10_000.0;
/// Hard cap on the number of remembered directories.
pub const MAX_ENTRIES: usize = 5_000;

const HOUR: u64 = 3600;
const DAY: u64 = 24 * HOUR;
const WEEK: u64 = 7 * DAY;

/// One remembered directory.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FrecencyEntry {
    pub path: String,
    pub rank: f64,
    /// Unix time of the last visit, in seconds.
    pub last: u64,
}

impl FrecencyEntry {
    /// The ranking score at time `now` (seconds since the Unix epoch).
    pub fn score(&self, now: u64) -> f64 {
        let age = now.saturating_sub(self.last);
        let factor = if age < HOUR {
            4.0
        } else if age < DAY {
            2.0
        } else if age < WEEK {
            0.5
        } else {
            0.25
        };
        self.rank * factor
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct FrecencyFile {
    #[serde(default)]
    entries: Vec<FrecencyEntry>,
}

/// The database. Paths are keyed case-insensitively on Windows and with `/` separators.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Frecency {
    entries: HashMap<String, FrecencyEntry>,
}

fn key_of(path: &str) -> String {
    let unified = path.replace('\\', "/");
    if cfg!(windows) {
        unified.to_lowercase()
    } else {
        unified
    }
}

impl Frecency {
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, path: &Path) -> Option<&FrecencyEntry> {
        self.entries.get(&key_of(path.to_str()?))
    }

    /// Records a visit. Paths that are not valid Unicode cannot be stored and are ignored.
    pub fn visit(&mut self, path: &Path, now: u64) {
        let Some(text) = path.to_str() else {
            return;
        };
        let entry = self
            .entries
            .entry(key_of(text))
            .or_insert_with(|| FrecencyEntry {
                path: text.to_owned(),
                rank: 0.0,
                last: now,
            });
        entry.path = text.to_owned();
        entry.rank += 1.0;
        entry.last = now;
        let key = key_of(text);
        self.age_if_needed(Some(&key));
    }

    /// Forgets a directory (it no longer exists).
    pub fn remove(&mut self, path: &Path) -> bool {
        path.to_str()
            .is_some_and(|p| self.entries.remove(&key_of(p)).is_some())
    }

    /// Ages the database and enforces the caps. `protect` (the key just visited) is never
    /// evicted by the entry cap; ties on rank are broken by recency, then by key.
    fn age_if_needed(&mut self, protect: Option<&str>) {
        let total: f64 = self.entries.values().map(|e| e.rank).sum();
        if total > MAX_TOTAL_RANK {
            for entry in self.entries.values_mut() {
                entry.rank *= 0.9;
            }
            self.entries
                .retain(|k, e| e.rank >= 1.0 || Some(k.as_str()) == protect);
        }
        if self.entries.len() > MAX_ENTRIES {
            let mut by_rank: Vec<(String, f64, u64)> = self
                .entries
                .iter()
                .map(|(k, e)| (k.clone(), e.rank, e.last))
                .collect();
            by_rank.sort_by(|a, b| {
                (Some(b.0.as_str()) == protect)
                    .cmp(&(Some(a.0.as_str()) == protect))
                    .then_with(|| b.1.total_cmp(&a.1))
                    .then_with(|| b.2.cmp(&a.2))
                    .then_with(|| a.0.cmp(&b.0))
            });
            for (key, _, _) in by_rank.into_iter().skip(MAX_ENTRIES) {
                self.entries.remove(&key);
            }
        }
    }

    /// All entries with their score at `now`, best first (ties by path, so the order is
    /// deterministic).
    pub fn ranked(&self, now: u64) -> Vec<(&FrecencyEntry, f64)> {
        let mut all: Vec<_> = self.entries.values().map(|e| (e, e.score(now))).collect();
        all.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.path.cmp(&b.0.path)));
        all
    }

    /// Merges a database loaded from disk into this one: ranks add up, the later visit wins.
    pub fn merge_loaded(&mut self, loaded: Frecency) {
        for (key, theirs) in loaded.entries {
            match self.entries.get_mut(&key) {
                Some(ours) => {
                    ours.rank += theirs.rank;
                    ours.last = ours.last.max(theirs.last);
                }
                None => {
                    self.entries.insert(key, theirs);
                }
            }
        }
        self.age_if_needed(None);
    }

    /// Three-way merge for concurrent instances: `base` is what this instance last synced with
    /// the file, `ours` its current database, `disk` what the file holds now. Visits are
    /// counters, so this instance's change since `base` (its rank delta) is added to the
    /// file's rank; entries it forgot are dropped; the later visit wins.
    pub fn merge_three_way(base: &Frecency, ours: &Frecency, disk: Frecency) -> Frecency {
        let mut merged = disk;
        // The directory this instance visited last must survive the cap below.
        let mut protect: Option<(&str, u64)> = None;
        for (key, mine) in &ours.entries {
            let before = base.entries.get(key);
            let delta = mine.rank - before.map_or(0.0, |b| b.rank);
            if delta > 0.0 && protect.is_none_or(|(_, last)| mine.last > last) {
                protect = Some((key.as_str(), mine.last));
            }
            match merged.entries.get_mut(key) {
                Some(theirs) => {
                    theirs.rank += delta;
                    theirs.last = theirs.last.max(mine.last);
                    if theirs.rank <= 0.0 {
                        merged.entries.remove(key);
                    }
                }
                None if before.is_none() => {
                    merged.entries.insert(key.clone(), mine.clone());
                }
                // The other instance forgot it; keep only what we added since.
                None if delta > 0.0 => {
                    let mut entry = mine.clone();
                    entry.rank = delta;
                    merged.entries.insert(key.clone(), entry);
                }
                None => {}
            }
        }
        for key in base.entries.keys() {
            if !ours.entries.contains_key(key) {
                merged.entries.remove(key);
            }
        }
        merged.age_if_needed(protect.map(|(key, _)| key));
        merged
    }

    /// The `frecency.toml` text (entries sorted by path for stable files).
    pub fn to_toml(&self) -> String {
        let mut entries: Vec<FrecencyEntry> = self.entries.values().cloned().collect();
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        toml::to_string(&FrecencyFile { entries }).unwrap_or_default()
    }

    /// Parses `frecency.toml`. Entries with a non-finite or non-positive rank are dropped.
    pub fn from_toml(text: &str) -> Result<Self, String> {
        let file: FrecencyFile = toml::from_str(text).map_err(|e| e.to_string())?;
        let mut db = Frecency::default();
        for entry in file.entries {
            if entry.path.is_empty() || !entry.rank.is_finite() || entry.rank <= 0.0 {
                continue;
            }
            db.entries.insert(key_of(&entry.path), entry);
        }
        Ok(db)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: u64 = 1_700_000_000;

    #[test]
    fn visits_accumulate_rank() {
        let mut db = Frecency::default();
        db.visit(Path::new("/a"), T0);
        db.visit(Path::new("/a"), T0 + 5);
        db.visit(Path::new("/b"), T0 + 6);
        assert_eq!(db.len(), 2);
        let a = db.get(Path::new("/a")).unwrap();
        assert_eq!((a.rank, a.last), (2.0, T0 + 5));
    }

    #[test]
    fn score_uses_the_time_buckets() {
        let e = |age: u64| FrecencyEntry {
            path: "/x".into(),
            rank: 10.0,
            last: T0 - age,
        };
        assert_eq!(e(0).score(T0), 40.0);
        assert_eq!(e(HOUR - 1).score(T0), 40.0);
        assert_eq!(e(HOUR).score(T0), 20.0);
        assert_eq!(e(DAY - 1).score(T0), 20.0);
        assert_eq!(e(DAY).score(T0), 5.0);
        assert_eq!(e(WEEK - 1).score(T0), 5.0);
        assert_eq!(e(WEEK).score(T0), 2.5);
        assert_eq!(e(400 * DAY).score(T0), 2.5);
        // A clock that went backwards counts as "just now".
        assert_eq!(e(0).score(T0 - 10), 40.0);
    }

    #[test]
    fn recent_beats_frequent_but_old() {
        let mut db = Frecency::default();
        for _ in 0..6 {
            db.visit(Path::new("/old"), T0 - 30 * DAY);
        }
        db.visit(Path::new("/new"), T0 - 60);
        db.visit(Path::new("/new"), T0 - 30);
        let order: Vec<_> = db.ranked(T0).iter().map(|(e, _)| e.path.clone()).collect();
        // old: 6 * 0.25 = 1.5, new: 2 * 4 = 8
        assert_eq!(order, ["/new", "/old"]);
    }

    #[test]
    fn aging_keeps_the_total_bounded_and_drops_the_weak() {
        let mut db = Frecency::default();
        db.visit(Path::new("/rare"), T0);
        for i in 0..11_000u64 {
            db.visit(Path::new("/busy"), T0 + i);
        }
        let total: f64 = db.ranked(T0).iter().map(|(e, _)| e.rank).sum();
        assert!(total <= MAX_TOTAL_RANK + 1.0, "total {total}");
        assert!(db.get(Path::new("/busy")).is_some());
        assert!(
            db.get(Path::new("/rare")).is_none(),
            "a rank of 1 ages below 1 and is dropped"
        );
    }

    #[test]
    fn entry_count_is_capped() {
        let mut db = Frecency::default();
        for i in 0..MAX_ENTRIES + 50 {
            db.visit(Path::new(&format!("/d{i}")), T0);
        }
        assert_eq!(db.len(), MAX_ENTRIES);
    }

    #[test]
    fn a_just_visited_directory_survives_the_entry_cap() {
        let mut db = Frecency::default();
        for i in 0..MAX_ENTRIES {
            db.visit(Path::new(&format!("/d{i:05}")), T0 + i as u64);
        }
        assert_eq!(db.len(), MAX_ENTRIES);
        // Sorts alphabetically last and has the lowest possible rank (1.0): it used to be the
        // first thing evicted.
        db.visit(Path::new("/zeta"), T0 + 1_000_000);
        assert!(db.get(Path::new("/zeta")).is_some(), "fresh visit evicted");
        assert_eq!(db.len(), MAX_ENTRIES);
        // Among equal ranks the least recently visited goes (d00000, visited first).
        assert!(db.get(Path::new("/d00000")).is_none());
        assert!(db.get(Path::new("/d00001")).is_some());
    }

    #[test]
    fn a_merge_never_evicts_the_latest_visit_of_the_writer() {
        let mut disk = Frecency::default();
        for i in 0..MAX_ENTRIES {
            disk.visit(Path::new(&format!("/d{i:05}")), T0 + i as u64);
        }
        let base = disk.clone();
        let mut ours = base.clone();
        // Sorts last, rank 1.0 (the lowest) and the newest visit.
        ours.visit(Path::new("/zeta"), T0 + 1_000_000);
        let merged = Frecency::merge_three_way(&base, &ours, disk);
        assert!(
            merged.get(Path::new("/zeta")).is_some(),
            "fresh visit evicted"
        );
        assert_eq!(merged.len(), MAX_ENTRIES);
    }

    #[test]
    fn three_way_merge_adds_rank_deltas_and_honours_forgetting() {
        let mut base = Frecency::default();
        base.visit(Path::new("/x"), T0);
        base.visit(Path::new("/gone"), T0);
        let mut ours = base.clone();
        ours.visit(Path::new("/x"), T0 + 10);
        ours.visit(Path::new("/new"), T0 + 10);
        ours.remove(Path::new("/gone"));
        let mut disk = base.clone();
        disk.visit(Path::new("/x"), T0 + 20);
        disk.visit(Path::new("/x"), T0 + 21);
        disk.visit(Path::new("/theirs"), T0 + 21);
        let merged = Frecency::merge_three_way(&base, &ours, disk);
        let x = merged.get(Path::new("/x")).unwrap();
        assert_eq!((x.rank, x.last), (4.0, T0 + 21));
        assert!(merged.get(Path::new("/new")).is_some());
        assert!(merged.get(Path::new("/theirs")).is_some());
        assert!(merged.get(Path::new("/gone")).is_none());
    }

    #[test]
    fn remove_forgets_a_directory() {
        let mut db = Frecency::default();
        db.visit(Path::new("/a"), T0);
        assert!(db.remove(Path::new("/a")));
        assert!(!db.remove(Path::new("/a")));
        assert!(db.is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths_are_case_and_separator_insensitive() {
        let mut db = Frecency::default();
        db.visit(Path::new(r"C:\Users\Me"), T0);
        db.visit(Path::new("c:/users/me"), T0 + 1);
        assert_eq!(db.len(), 1);
        assert_eq!(db.get(Path::new(r"C:\USERS\ME")).unwrap().rank, 2.0);
    }

    #[test]
    fn persistence_round_trip() {
        let mut db = Frecency::default();
        db.visit(Path::new("/a b/ü"), T0);
        db.visit(Path::new("/a b/ü"), T0 + 1);
        db.visit(Path::new("/c"), T0 + 2);
        let back = Frecency::from_toml(&db.to_toml()).unwrap();
        assert_eq!(back, db);
    }

    #[test]
    fn corrupt_files_are_errors_and_bad_entries_are_skipped() {
        assert!(Frecency::from_toml("[[entries]]\npath = ").is_err());
        assert!(Frecency::from_toml("[[entries]]\npath = \"/a\"\nrank = \"x\"\nlast = 1").is_err());
        let db = Frecency::from_toml(
            "[[entries]]\npath = \"\"\nrank = 1.0\nlast = 1\n\
             [[entries]]\npath = \"/neg\"\nrank = -2.0\nlast = 1\n\
             [[entries]]\npath = \"/ok\"\nrank = 2.5\nlast = 9\n",
        )
        .unwrap();
        assert_eq!(db.len(), 1);
        assert!(Frecency::from_toml("").unwrap().is_empty());
    }

    #[test]
    fn merging_a_loaded_database_adds_ranks() {
        let mut ours = Frecency::default();
        ours.visit(Path::new("/a"), T0 + 10);
        let mut loaded = Frecency::default();
        loaded.visit(Path::new("/a"), T0);
        loaded.visit(Path::new("/b"), T0);
        ours.merge_loaded(loaded);
        let a = ours.get(Path::new("/a")).unwrap();
        assert_eq!((a.rank, a.last), (2.0, T0 + 10));
        assert!(ours.get(Path::new("/b")).is_some());
    }
}
