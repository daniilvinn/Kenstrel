//! Bookmarks (`m<char>` / `'<char>`): single characters naming directories.
//!
//! Pure data plus the text form stored in `bookmarks.toml`; reading and writing the file is
//! the application's job.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The bookmark table. Keys are single characters.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Bookmarks {
    map: BTreeMap<char, PathBuf>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct BookmarkFile {
    #[serde(default)]
    bookmarks: BTreeMap<String, String>,
}

impl Bookmarks {
    pub fn get(&self, key: char) -> Option<&Path> {
        self.map.get(&key).map(PathBuf::as_path)
    }

    /// Sets `key`, returning the directory it used to name.
    pub fn set(&mut self, key: char, path: PathBuf) -> Option<PathBuf> {
        self.map.insert(key, path)
    }

    pub fn remove(&mut self, key: char) -> Option<PathBuf> {
        self.map.remove(&key)
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Bookmarks ordered by key.
    pub fn iter(&self) -> impl Iterator<Item = (char, &Path)> {
        self.map.iter().map(|(k, v)| (*k, v.as_path()))
    }

    /// The `index`-th bookmark in key order.
    pub fn nth(&self, index: usize) -> Option<(char, &Path)> {
        self.iter().nth(index)
    }

    /// Adds the bookmarks of `loaded` that are not set yet (what the user set before the
    /// file finished loading wins).
    pub fn merge_loaded(&mut self, loaded: Bookmarks) {
        for (key, path) in loaded.map {
            self.map.entry(key).or_insert(path);
        }
    }

    /// The `bookmarks.toml` text. Paths that are not valid Unicode cannot be stored and are
    /// left out (they cannot be created either, see `set_bookmark`).
    pub fn to_toml(&self) -> String {
        let file = BookmarkFile {
            bookmarks: self
                .map
                .iter()
                .filter_map(|(k, v)| Some((k.to_string(), v.to_str()?.to_owned())))
                .collect(),
        };
        toml::to_string(&file).unwrap_or_default()
    }

    /// Parses `bookmarks.toml`. Entries whose key is not a single character are skipped;
    /// a file that is not valid TOML of the expected shape is an error.
    pub fn from_toml(text: &str) -> Result<Self, String> {
        let file: BookmarkFile = toml::from_str(text).map_err(|e| e.to_string())?;
        let map = file
            .bookmarks
            .into_iter()
            .filter_map(|(key, path)| {
                let mut chars = key.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) if !c.is_control() && !path.is_empty() => {
                        Some((c, PathBuf::from(path)))
                    }
                    _ => None,
                }
            })
            .collect();
        Ok(Self { map })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_keeps_every_entry() {
        let mut b = Bookmarks::default();
        b.set('a', PathBuf::from(r"C:\Users\me\code"));
        b.set('Z', PathBuf::from("/tmp/with space"));
        b.set('é', PathBuf::from("/tmp/ü"));
        b.set('"', PathBuf::from("/q"));
        let back = Bookmarks::from_toml(&b.to_toml()).unwrap();
        assert_eq!(back, b);
    }

    #[test]
    fn corrupt_or_odd_files() {
        assert!(Bookmarks::from_toml("this is = = not toml").is_err());
        assert!(Bookmarks::from_toml("[bookmarks]\na = 5").is_err());
        let b = Bookmarks::from_toml("[bookmarks]\nab = \"/x\"\nc = \"\"\nd = \"/ok\"").unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!(b.get('d'), Some(Path::new("/ok")));
        assert!(Bookmarks::from_toml("").unwrap().is_empty());
    }

    #[test]
    fn merge_prefers_what_is_already_set() {
        let mut b = Bookmarks::default();
        b.set('a', PathBuf::from("/new"));
        let mut loaded = Bookmarks::default();
        loaded.set('a', PathBuf::from("/old"));
        loaded.set('b', PathBuf::from("/b"));
        b.merge_loaded(loaded);
        assert_eq!(b.get('a'), Some(Path::new("/new")));
        assert_eq!(b.get('b'), Some(Path::new("/b")));
        assert_eq!(b.nth(1).map(|(k, _)| k), Some('b'));
    }
}
