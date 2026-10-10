//! Git status data as the UI consumes it. Producing it (running `git`, parsing porcelain v2)
//! is `ks-fs`'s job; this module only holds and queries the result.

use std::collections::HashMap;

/// The state shown as a glyph next to an entry. Variants are ordered by display priority:
/// when a directory contains several states, the greatest one wins.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GitState {
    Ignored,
    Untracked,
    Added,
    Renamed,
    Deleted,
    Modified,
    Conflict,
}

impl GitState {
    /// The glyph for this state. Plain ASCII so it renders in every font.
    pub const fn glyph(self) -> char {
        match self {
            GitState::Ignored => '!',
            GitState::Untracked => '?',
            GitState::Added => '+',
            GitState::Renamed => 'R',
            GitState::Deleted => 'D',
            GitState::Modified => 'M',
            GitState::Conflict => 'U',
        }
    }
}

/// `# branch.*` headers of `git status --porcelain=v2 --branch`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GitBranch {
    /// Branch name; `None` when HEAD is detached.
    pub name: Option<String>,
    /// Abbreviated commit id (shown instead of the name when detached).
    pub oid: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
}

impl GitBranch {
    /// Branch name, or `(abc1234)` when detached.
    pub fn label(&self) -> String {
        match (&self.name, &self.oid) {
            (Some(name), _) => name.clone(),
            (None, Some(oid)) => format!("({oid})"),
            (None, None) => "(detached)".to_owned(),
        }
    }
}

/// Repository-wide change counts for the header.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GitCounts {
    /// Paths with changes in the index.
    pub staged: u32,
    /// Paths with changes in the work tree (not counting untracked files).
    pub modified: u32,
    pub untracked: u32,
    pub conflicted: u32,
}

impl GitCounts {
    pub const fn is_clean(&self) -> bool {
        self.staged == 0 && self.modified == 0 && self.untracked == 0 && self.conflicted == 0
    }
}

/// One path with a state, relative to the repository root, `/` separated. A directory
/// (collapsed untracked or ignored directory) has a trailing `/`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitRecord {
    pub path: String,
    pub state: GitState,
}

/// The git status of one directory listing: a state per child of the directory, with the
/// states of everything below a child folded into it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GitSnapshot {
    pub branch: GitBranch,
    pub counts: GitCounts,
    children: HashMap<String, GitState>,
    /// State inherited from a collapsed untracked/ignored ancestor directory.
    inherited: Option<GitState>,
}

impl GitSnapshot {
    /// Builds the snapshot for the directory `prefix` (its path relative to the repository
    /// root, `""` for the root itself, otherwise ending in `/`).
    pub fn new(
        branch: GitBranch,
        counts: GitCounts,
        prefix: &str,
        records: impl IntoIterator<Item = GitRecord>,
    ) -> Self {
        let mut children: HashMap<String, GitState> = HashMap::new();
        let mut inherited: Option<GitState> = None;
        for GitRecord { path, state } in records {
            let path = path.trim_end_matches('/');
            if let Some(rest) = path.strip_prefix(prefix) {
                let (child, deeper) = match rest.split_once('/') {
                    Some((child, _)) => (child, true),
                    None => (rest, false),
                };
                // Ignored files do not make their parent directory look changed.
                if child.is_empty() || (deeper && state == GitState::Ignored) {
                    continue;
                }
                let slot = children.entry(child.to_owned()).or_insert(state);
                *slot = (*slot).max(state);
            } else if matches!(state, GitState::Untracked | GitState::Ignored)
                && prefix.starts_with(path)
                && prefix[path.len()..].starts_with('/')
            {
                // The whole directory we are looking at lies inside a collapsed directory.
                inherited = Some(state);
            }
        }
        Self {
            branch,
            counts,
            children,
            inherited,
        }
    }

    /// The state of the child called `name`.
    pub fn state_of(&self, name: &str) -> Option<GitState> {
        self.children.get(name).copied().or(self.inherited)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(path: &str, state: GitState) -> GitRecord {
        GitRecord {
            path: path.to_owned(),
            state,
        }
    }

    fn snapshot(prefix: &str, records: Vec<GitRecord>) -> GitSnapshot {
        GitSnapshot::new(GitBranch::default(), GitCounts::default(), prefix, records)
    }

    #[test]
    fn states_aggregate_into_child_directories_with_priority() {
        let snap = snapshot(
            "",
            vec![
                rec("src/a.rs", GitState::Added),
                rec("src/deep/b.rs", GitState::Modified),
                rec("src/c.rs", GitState::Untracked),
                rec("README.md", GitState::Modified),
                rec("docs/x.md", GitState::Added),
            ],
        );
        assert_eq!(snap.state_of("src"), Some(GitState::Modified));
        assert_eq!(snap.state_of("docs"), Some(GitState::Added));
        assert_eq!(snap.state_of("README.md"), Some(GitState::Modified));
        assert_eq!(snap.state_of("clean.txt"), None);
    }

    #[test]
    fn conflicts_outrank_everything() {
        let snap = snapshot(
            "",
            vec![
                rec("d/a", GitState::Modified),
                rec("d/b", GitState::Conflict),
                rec("d/c", GitState::Deleted),
            ],
        );
        assert_eq!(snap.state_of("d"), Some(GitState::Conflict));
    }

    #[test]
    fn only_the_current_directorys_children_are_kept() {
        let snap = snapshot(
            "src/",
            vec![
                rec("src/a.rs", GitState::Modified),
                rec("src/inner/b.rs", GitState::Added),
                rec("other/c.rs", GitState::Modified),
                rec("srcx/d.rs", GitState::Modified),
            ],
        );
        assert_eq!(snap.state_of("a.rs"), Some(GitState::Modified));
        assert_eq!(snap.state_of("inner"), Some(GitState::Added));
        assert_eq!(snap.state_of("other"), None);
        assert_eq!(snap.state_of("c.rs"), None);
        assert_eq!(snap.state_of("d.rs"), None);
    }

    #[test]
    fn ignored_entries_do_not_taint_their_parent() {
        let snap = snapshot(
            "",
            vec![
                rec("target/", GitState::Ignored),
                rec("pkg/build/out.o", GitState::Ignored),
            ],
        );
        assert_eq!(snap.state_of("target"), Some(GitState::Ignored));
        assert_eq!(snap.state_of("pkg"), None);
    }

    #[test]
    fn collapsed_ancestors_are_inherited() {
        let snap = snapshot(
            "node_modules/pkg/",
            vec![rec("node_modules/", GitState::Ignored)],
        );
        assert_eq!(snap.state_of("index.js"), Some(GitState::Ignored));
        let snap = snapshot(
            "newdir/",
            vec![
                rec("newdir/", GitState::Untracked),
                rec("newdir/tracked", GitState::Modified),
            ],
        );
        assert_eq!(snap.state_of("anything"), Some(GitState::Untracked));
        assert_eq!(snap.state_of("tracked"), Some(GitState::Modified));
    }

    #[test]
    fn a_sibling_with_a_shared_name_prefix_is_not_an_ancestor() {
        let snap = snapshot("app/src/", vec![rec("app/", GitState::Untracked)]);
        assert_eq!(snap.state_of("x"), Some(GitState::Untracked));
        let snap = snapshot("apple/src/", vec![rec("app/", GitState::Untracked)]);
        assert_eq!(snap.state_of("x"), None);
    }

    #[test]
    fn branch_label() {
        let named = GitBranch {
            name: Some("main".into()),
            ..GitBranch::default()
        };
        assert_eq!(named.label(), "main");
        let detached = GitBranch {
            oid: Some("abc1234".into()),
            ..GitBranch::default()
        };
        assert_eq!(detached.label(), "(abc1234)");
        assert_eq!(GitBranch::default().label(), "(detached)");
    }

    #[test]
    fn glyphs_are_distinct() {
        let all = [
            GitState::Ignored,
            GitState::Untracked,
            GitState::Added,
            GitState::Renamed,
            GitState::Deleted,
            GitState::Modified,
            GitState::Conflict,
        ];
        let glyphs: std::collections::HashSet<char> = all.iter().map(|s| s.glyph()).collect();
        assert_eq!(glyphs.len(), all.len());
    }
}
