//! Synthetic data generators for benchmarks and large tests. Not used by the engines.

use std::fs;
use std::io::Write;
use std::path::Path;

/// Tiny deterministic xorshift PRNG (no `rand` dependency).
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    /// Seeded generator (seed must be non-zero; zero is bumped).
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    /// Next pseudo-random value.
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// Value in `0..n`.
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
}

const WORDS: &[&str] = &[
    "src", "lib", "main", "util", "core", "parser", "render", "config", "index", "model", "view",
    "test", "mod", "handler", "service", "client", "server", "buffer", "stream", "cache", "walker",
    "matcher", "search", "filter", "event", "state", "theme", "input", "output", "network",
    "storage", "worker",
];
const EXTS: &[&str] = &[
    "rs", "toml", "md", "txt", "json", "ts", "py", "c", "h", "yml",
];

/// Generates `n` synthetic relative paths such as `src/parser/handler_cache.rs` (about 4 levels
/// deep, realistic distribution).
pub fn synthetic_paths(n: usize, seed: u64) -> Vec<String> {
    let mut rng = Rng::new(seed);
    let sep = std::path::MAIN_SEPARATOR;
    (0..n)
        .map(|i| {
            let depth = 1 + rng.below(5);
            let mut p = String::with_capacity(64);
            for _ in 0..depth {
                p.push_str(WORDS[rng.below(WORDS.len())]);
                p.push_str(&(rng.below(40)).to_string());
                p.push(sep);
            }
            p.push_str(WORDS[rng.below(WORDS.len())]);
            p.push('_');
            p.push_str(WORDS[rng.below(WORDS.len())]);
            p.push_str(&i.to_string());
            p.push('.');
            p.push_str(EXTS[rng.below(EXTS.len())]);
            p
        })
        .collect()
}

/// Creates a tree of `files` files under `root` (about 100 files per directory, mixed sizes
/// from 100 B to 100 KB, text lines drawn from a vocabulary; roughly one line in 500 contains
/// the marker `NEEDLE_MARKER`). Returns the number of bytes written.
pub fn generate_tree(root: &Path, files: usize, seed: u64) -> std::io::Result<u64> {
    let mut rng = Rng::new(seed);
    let mut written = 0u64;
    let mut line = String::new();
    for i in 0..files {
        let dir = root
            .join(format!("d{}", (i / 100) % 50))
            .join(format!("s{}", i / 5000))
            .join(format!("p{}", i / 100));
        if i % 100 == 0 {
            fs::create_dir_all(&dir)?;
        }
        let size_class = rng.below(100);
        let target = match size_class {
            0..=59 => 100 + rng.below(2_000),
            60..=89 => 2_000 + rng.below(20_000),
            _ => 20_000 + rng.below(80_000),
        };
        let mut f = std::io::BufWriter::new(fs::File::create(dir.join(format!("f{i}.txt")))?);
        let mut size = 0usize;
        let mut n = 0u32;
        while size < target {
            line.clear();
            for _ in 0..(4 + rng.below(8)) {
                line.push_str(WORDS[rng.below(WORDS.len())]);
                line.push(' ');
            }
            if rng.below(500) == 0 {
                line.push_str("NEEDLE_MARKER ");
            }
            line.push_str(&format!("line{n}\n"));
            f.write_all(line.as_bytes())?;
            size += line.len();
            n += 1;
        }
        f.flush()?;
        written += size as u64;
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_deterministic_and_unique_enough() {
        let a = synthetic_paths(1000, 7);
        let b = synthetic_paths(1000, 7);
        assert_eq!(a, b);
        let set: std::collections::HashSet<_> = a.iter().collect();
        assert_eq!(set.len(), a.len());
    }

    #[test]
    fn tree_generates() {
        let t = tempfile::tempdir().unwrap();
        let bytes = generate_tree(t.path(), 250, 1).unwrap();
        assert!(bytes > 0);
    }
}
