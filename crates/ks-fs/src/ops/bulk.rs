//! Bulk rename: edit-file generation, parsing, validation and plan building.
//!
//! Flow used by the app (`R`):
//! 1. [`BulkRename::new`] with the directory and the names of the selected items.
//! 2. [`BulkRename::write_edit_file`] writes the names (one per line) to a temp file.
//! 3. The app opens `$EDITOR` on it (see `launch::editor`) and waits.
//! 4. [`BulkRename::plan_from_file`] / [`BulkRename::plan`] parse and validate the edited text and
//!    return a [`RenamePlan`] whose [`RenamePlan::preview`] is shown for confirmation.
//! 5. [`RenamePlan::into_spec`] yields the job to submit; it is undoable as one entry.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};

use super::names::{validate_name, NameError};
use super::types::{JobSpec, TransferItem};

/// A problem found while validating the edited file. Line numbers are 1-based.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BulkError {
    /// The number of lines changed.
    #[error("expected {expected} lines but found {found}")]
    LineCount {
        /// Number of selected items.
        expected: usize,
        /// Lines in the edited file.
        found: usize,
    },
    /// A line is empty.
    #[error("line {line}: name is empty")]
    Empty {
        /// Line number.
        line: usize,
    },
    /// The same new name appears twice.
    #[error("line {line}: duplicate name `{name}`")]
    Duplicate {
        /// Line number.
        line: usize,
        /// The name.
        name: String,
    },
    /// The name is not valid on Windows.
    #[error("line {line}: `{name}`: {error}")]
    Invalid {
        /// Line number.
        line: usize,
        /// The name.
        name: String,
        /// Reason.
        error: NameError,
    },
    /// The name is taken by an item that is not part of the rename.
    #[error("line {line}: `{name}` already exists")]
    Collision {
        /// Line number.
        line: usize,
        /// The name.
        name: String,
    },
}

/// One effective rename (unchanged lines are not included).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameChange {
    /// Old name.
    pub from: String,
    /// New name.
    pub to: String,
}

/// Validated, ordered rename plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenamePlan {
    /// Directory containing the items.
    pub dir: PathBuf,
    /// The effective renames, in list order (for the preview).
    pub changes: Vec<RenameChange>,
    /// Executable steps in order. Swaps and cycles go through temporary names, so this can be
    /// longer than `changes`.
    pub steps: Vec<TransferItem>,
}

impl RenamePlan {
    /// Nothing to do.
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// Preview lines `old -> new` for the confirmation overlay.
    pub fn preview(&self) -> Vec<String> {
        self.changes
            .iter()
            .map(|c| format!("{} -> {}", c.from, c.to))
            .collect()
    }

    /// The job that executes the plan.
    pub fn into_spec(self) -> JobSpec {
        JobSpec::BulkRename { steps: self.steps }
    }
}

/// A bulk-rename session for one directory.
///
/// Names are `String`s because they go through a text file; an item whose name is not valid
/// Unicode cannot round-trip through the editor, so callers should leave such items out of the
/// selection (they are reported as unsupported by the wiring).
#[derive(Debug, Clone)]
pub struct BulkRename {
    dir: PathBuf,
    originals: Vec<String>,
}

fn key(name: &str) -> String {
    if cfg!(windows) {
        name.to_lowercase()
    } else {
        name.to_owned()
    }
}

impl BulkRename {
    /// Session for the items `originals` (file names) in `dir`.
    pub fn new(dir: impl Into<PathBuf>, originals: Vec<String>) -> Self {
        Self {
            dir: dir.into(),
            originals,
        }
    }

    /// The text to put in the edit file.
    pub fn edit_text(&self) -> String {
        let mut s = self.originals.join("\n");
        s.push('\n');
        s
    }

    /// Write [`Self::edit_text`] to a fresh temp file and return its path. The caller removes
    /// the file after planning.
    pub fn write_edit_file(&self) -> io::Result<PathBuf> {
        use std::io::Write;
        // Unpredictable name, created exclusively: never follows a pre-planted link.
        let mut f = tempfile::Builder::new()
            .prefix("kestrel-bulk-rename-")
            .suffix(".txt")
            .tempfile()?;
        f.write_all(self.edit_text().as_bytes())?;
        f.flush()?;
        let (_file, path) = f.keep().map_err(|e| e.error)?;
        Ok(path)
    }

    /// Read the edited file and plan.
    pub fn plan_from_file(&self, path: &Path) -> io::Result<Result<RenamePlan, Vec<BulkError>>> {
        let text = std::fs::read_to_string(path)?;
        Ok(self.plan(&text))
    }

    /// Parse and validate edited text, then build the plan. All problems are reported at once.
    pub fn plan(&self, edited: &str) -> Result<RenamePlan, Vec<BulkError>> {
        let edited = edited.strip_prefix('\u{feff}').unwrap_or(edited);
        let mut lines: Vec<&str> = edited
            .split('\n')
            .map(|l| l.strip_suffix('\r').unwrap_or(l))
            .collect();
        if lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop(); // the final newline
        }
        // Editors terminate the last line with a newline; drop trailing blanks beyond the count.
        while lines.len() > self.originals.len() && lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        if lines.len() != self.originals.len() {
            return Err(vec![BulkError::LineCount {
                expected: self.originals.len(),
                found: lines.len(),
            }]);
        }
        let mut errors = Vec::new();
        let original_keys: HashSet<String> = self.originals.iter().map(|n| key(n)).collect();
        let mut seen: HashMap<String, usize> = HashMap::new();
        for (i, (new, old)) in lines.iter().zip(&self.originals).enumerate() {
            let line = i + 1;
            if new.is_empty() {
                errors.push(BulkError::Empty { line });
                continue;
            }
            if let Err(error) = validate_name(new) {
                errors.push(BulkError::Invalid {
                    line,
                    name: (*new).to_owned(),
                    error,
                });
                continue;
            }
            if seen.insert(key(new), line).is_some() {
                errors.push(BulkError::Duplicate {
                    line,
                    name: (*new).to_owned(),
                });
                continue;
            }
            if *new != old
                && !original_keys.contains(&key(new))
                && std::fs::symlink_metadata(self.dir.join(new)).is_ok()
            {
                errors.push(BulkError::Collision {
                    line,
                    name: (*new).to_owned(),
                });
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        let changes: Vec<RenameChange> = self
            .originals
            .iter()
            .zip(&lines)
            .filter(|(a, b)| a.as_str() != **b)
            .map(|(a, b)| RenameChange {
                from: a.clone(),
                to: (*b).to_owned(),
            })
            .collect();
        let steps = self.order_steps(&changes);
        Ok(RenamePlan {
            dir: self.dir.clone(),
            changes,
            steps,
        })
    }

    /// Order renames so no step overwrites a name that is still needed; break cycles (including
    /// swaps) by moving one member to a temporary name first.
    fn order_steps(&self, changes: &[RenameChange]) -> Vec<TransferItem> {
        let mut taken: HashSet<String> = self.originals.iter().map(|n| key(n)).collect();
        taken.extend(changes.iter().map(|c| key(&c.to)));
        let mut pending: Vec<(String, String)> = changes
            .iter()
            .map(|c| (c.from.clone(), c.to.clone()))
            .collect();
        let mut order: Vec<(String, String)> = Vec::new();
        let mut tmp_n = 0u32;
        while !pending.is_empty() {
            let froms: HashSet<String> = pending.iter().map(|(f, _)| key(f)).collect();
            let (ready, rest): (Vec<_>, Vec<_>) = pending
                .into_iter()
                .partition(|(f, t)| key(t) == key(f) || !froms.contains(&key(t)));
            pending = rest;
            if !ready.is_empty() {
                order.extend(ready);
                continue;
            }
            // Every remaining target is some remaining source: a cycle. Park the first one.
            let (from, to) = pending.remove(0);
            let tmp = loop {
                tmp_n += 1;
                let candidate = format!(".ks-tmp-{tmp_n}");
                if !taken.contains(&key(&candidate))
                    && std::fs::symlink_metadata(self.dir.join(&candidate)).is_err()
                {
                    break candidate;
                }
            };
            taken.insert(key(&tmp));
            order.push((from, tmp.clone()));
            pending.push((tmp, to));
        }
        order
            .into_iter()
            .map(|(f, t)| TransferItem::new(self.dir.join(f), self.dir.join(t)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(names: &[&str]) -> (tempfile::TempDir, BulkRename) {
        let td = tempfile::tempdir().unwrap();
        for n in names {
            std::fs::write(td.path().join(n), n).unwrap();
        }
        let b = BulkRename::new(td.path(), names.iter().map(|s| s.to_string()).collect());
        (td, b)
    }

    #[test]
    fn simple_plan_and_preview() {
        let (_td, b) = session(&["a.txt", "b.txt", "c.txt"]);
        let plan = b.plan("a.txt\nbeta.txt\ngamma.txt\n").unwrap();
        assert_eq!(
            plan.preview(),
            vec!["b.txt -> beta.txt", "c.txt -> gamma.txt"]
        );
        assert_eq!(plan.steps.len(), 2);
        assert!(b.plan(&b.edit_text()).unwrap().is_empty());
    }

    #[test]
    fn validation_errors() {
        let (_td, b) = session(&["a", "b", "c"]);
        assert_eq!(
            b.plan("a\nb\n"),
            Err(vec![BulkError::LineCount {
                expected: 3,
                found: 2
            }])
        );
        let errs = b.plan("x\n\ny\n").unwrap_err();
        assert!(errs.contains(&BulkError::Empty { line: 2 }));
        assert!(errs
            .iter()
            .all(|e| !matches!(e, BulkError::Duplicate { .. })));
        let errs = b.plan("x\nX\ny\n").unwrap_err();
        if cfg!(windows) {
            assert!(errs.contains(&BulkError::Duplicate {
                line: 2,
                name: "X".into()
            }));
        }
        let errs = b.plan("a:b\nCON\nok.\n").unwrap_err();
        assert_eq!(errs.len(), 3);
    }

    #[test]
    fn collision_with_non_participant() {
        let (td, b) = session(&["a", "b"]);
        std::fs::write(td.path().join("other"), "x").unwrap();
        let errs = b.plan("other\nb\n").unwrap_err();
        assert_eq!(
            errs,
            vec![BulkError::Collision {
                line: 1,
                name: "other".into()
            }]
        );
    }

    #[test]
    fn crlf_and_trailing_blank_lines() {
        let (_td, b) = session(&["a", "b"]);
        let plan = b.plan("x\r\ny\r\n\r\n").unwrap();
        assert_eq!(plan.changes.len(), 2);
    }

    /// Apply steps on a real directory, checking no step clobbers a file.
    fn apply(dir: &Path, plan: &RenamePlan) {
        for s in &plan.steps {
            assert!(
                !s.dst.exists() || key(&s.src.to_string_lossy()) == key(&s.dst.to_string_lossy())
            );
            std::fs::rename(&s.src, &s.dst).unwrap();
        }
        let _ = dir;
    }

    #[test]
    fn swap_uses_temp_name() {
        let (td, b) = session(&["a", "b"]);
        let plan = b.plan("b\na\n").unwrap();
        assert_eq!(plan.steps.len(), 3);
        apply(td.path(), &plan);
        assert_eq!(std::fs::read_to_string(td.path().join("a")).unwrap(), "b");
        assert_eq!(std::fs::read_to_string(td.path().join("b")).unwrap(), "a");
    }

    #[test]
    fn three_cycle_and_chain() {
        let (td, b) = session(&["a", "b", "c", "d"]);
        // a->b, b->c, c->a (cycle) and d->e
        let plan = b.plan("b\nc\na\ne\n").unwrap();
        apply(td.path(), &plan);
        assert_eq!(std::fs::read_to_string(td.path().join("b")).unwrap(), "a");
        assert_eq!(std::fs::read_to_string(td.path().join("c")).unwrap(), "b");
        assert_eq!(std::fs::read_to_string(td.path().join("a")).unwrap(), "c");
        assert_eq!(std::fs::read_to_string(td.path().join("e")).unwrap(), "d");
        assert!(!td.path().join(".ks-tmp-1").exists());
    }

    #[test]
    fn chain_without_temp() {
        let (td, b) = session(&["a", "b"]);
        // a->b, b->c: must run b->c first, no temp needed.
        let plan = b.plan("b\nc\n").unwrap();
        assert_eq!(plan.steps.len(), 2);
        apply(td.path(), &plan);
        assert_eq!(std::fs::read_to_string(td.path().join("c")).unwrap(), "b");
    }

    #[test]
    fn case_only_rename() {
        let (td, b) = session(&["abc"]);
        let plan = b.plan("ABC\n").unwrap();
        assert_eq!(plan.steps.len(), 1);
        apply(td.path(), &plan);
        let names: Vec<_> = std::fs::read_dir(td.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("ABC")]);
    }

    #[test]
    fn edit_file_roundtrip() {
        let (_td, b) = session(&["x", "y"]);
        let p = b.write_edit_file().unwrap();
        std::fs::write(&p, "x\nz\n").unwrap();
        let plan = b.plan_from_file(&p).unwrap().unwrap();
        std::fs::remove_file(&p).unwrap();
        assert_eq!(plan.preview(), vec!["y -> z"]);
    }
}
