//! File-system work behind Claude's MCP tools, run on the blocking pool.
//!
//! Requests that only need application state are answered by `update` itself; what is left
//! here uses the real engines: `ks-search` (the same walker, fuzzy matcher and content search
//! the finder uses, headless and bounded by a limit) and `ks-fs` (directory listing).

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ks_core::claude::{ClaudeDirEntry, ClaudeHit, ClaudeRequest, ClaudeResponse, IpcContext};
use ks_search::{
    spawn_search, CancellationToken, FinderOptions, FinderSession, GrepEvent, GrepQuery, WalkCache,
};

/// Entries returned by `list_dir` at most.
const MAX_LIST_ENTRIES: usize = 2000;
/// Entries read from a directory before `list_dir` gives up counting.
const MAX_LIST_SCAN: usize = 50_000;
/// How long a file search may take before it answers with what it has.
const FILE_SEARCH_BUDGET: Duration = Duration::from_secs(8);
/// How long a content search may take before it is cancelled.
/// Below the IPC reply timeout (15 s), so a slow search answers with its partial hits.
const CONTENT_SEARCH_BUDGET: Duration = Duration::from_secs(12);
const MAX_FILE_LIMIT: usize = 1000;
const MAX_CONTENT_LIMIT: usize = 2000;

/// Result of one piece of work.
#[derive(Debug)]
pub struct Outcome {
    pub response: ClaudeResponse,
    /// For a successful `reveal`: the path the explorer should show.
    pub reveal: Option<PathBuf>,
}

impl Outcome {
    fn reply(response: ClaudeResponse) -> Self {
        Self {
            response,
            reveal: None,
        }
    }

    fn error(message: String) -> Self {
        Self::reply(ClaudeResponse::Error { message })
    }
}

/// Resolves `path` against the project root.
pub fn resolve(root: &Path, path: &str) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        root.join(p)
    }
}

fn display(path: &Path) -> String {
    dunce::simplified(path).to_string_lossy().into_owned()
}

/// Performs `request`. Requests `update` answers itself yield an error here (they never arrive).
pub fn run(
    request: &ClaudeRequest,
    ctx: &IpcContext,
    cache: &Arc<WalkCache>,
    cancel: &CancellationToken,
) -> Outcome {
    match request {
        ClaudeRequest::ListDir { path } => list_dir(&resolve(&ctx.root, path), cancel),
        ClaudeRequest::Reveal { path } => reveal(&resolve(&ctx.root, path)),
        ClaudeRequest::SearchFiles { query, root, limit } => {
            let root = root
                .as_deref()
                .map_or_else(|| ctx.root.clone(), |r| resolve(&ctx.root, r));
            search_files(&root, query, *limit, ctx, cache, cancel)
        }
        ClaudeRequest::SearchContents {
            pattern,
            glob,
            regex,
            limit,
        } => search_contents(
            &ctx.root,
            pattern,
            glob.as_deref(),
            *regex,
            *limit,
            ctx,
            cancel,
        ),
        ClaudeRequest::GetSelection | ClaudeRequest::HookEvent { .. } => {
            Outcome::error("request is answered by the application state".to_owned())
        }
    }
}

/// `reveal`: the explorer shows `path` if it exists.
pub fn reveal(path: &Path) -> Outcome {
    if std::fs::symlink_metadata(path).is_ok() {
        Outcome {
            response: ClaudeResponse::Ok,
            reveal: Some(path.to_path_buf()),
        }
    } else {
        Outcome::error(format!("{} does not exist", path.display()))
    }
}

/// `list_dir`: directories first, then files, each sorted case-insensitively.
pub fn list_dir(path: &Path, cancel: &CancellationToken) -> Outcome {
    let mut entries: Vec<ClaudeDirEntry> = Vec::new();
    let result = ks_fs::list_dir(path, cancel, |chunk| {
        entries.extend(chunk.into_iter().map(|e| ClaudeDirEntry {
            name: e.display_name().into_owned(),
            is_dir: e.is_dir,
            size: (!e.is_dir).then_some(e.size),
        }));
        entries.len() < MAX_LIST_SCAN
    });
    if let Err(e) = result {
        return Outcome::error(format!("cannot list {}: {e}", path.display()));
    }
    entries.sort_by_cached_key(|e| (!e.is_dir, e.name.to_lowercase()));
    if entries.len() > MAX_LIST_ENTRIES {
        // The protocol has no `truncated` flag, so say it in the listing itself.
        let total = entries.len();
        entries.truncate(MAX_LIST_ENTRIES);
        let scanned_all = total < MAX_LIST_SCAN;
        entries.push(ClaudeDirEntry {
            name: format!(
                "... listing truncated: showing {MAX_LIST_ENTRIES} of {}{} entries",
                total,
                if scanned_all { "" } else { "+" }
            ),
            is_dir: false,
            size: None,
        });
    }
    Outcome::reply(ClaudeResponse::Dir { entries })
}

/// `search_files`: the finder's walker and matcher, run headless.
pub fn search_files(
    root: &Path,
    query: &str,
    limit: usize,
    ctx: &IpcContext,
    cache: &Arc<WalkCache>,
    cancel: &CancellationToken,
) -> Outcome {
    search_files_with_budget(root, query, limit, ctx, cache, cancel, FILE_SEARCH_BUDGET)
}

/// [`search_files`] with an explicit wall-clock budget (tests widen or shrink it).
fn search_files_with_budget(
    root: &Path,
    query: &str,
    limit: usize,
    ctx: &IpcContext,
    cache: &Arc<WalkCache>,
    cancel: &CancellationToken,
    budget: Duration,
) -> Outcome {
    if !root.is_dir() {
        return Outcome::error(format!("{} is not a directory", root.display()));
    }
    let limit = limit.clamp(1, MAX_FILE_LIMIT);
    let options = FinderOptions {
        show_hidden: ctx.show_hidden,
        respect_gitignore: ctx.respect_gitignore,
        // This host clears its own cache on hook events and after a TTL; a reply must not wait
        // for a background re-walk.
        refresh_cached: false,
        ..FinderOptions::new(root)
    };
    let token = cancel.child_token();
    let mut session = FinderSession::start_with_token(
        options,
        0,
        Some(Arc::clone(cache)),
        Arc::new(|| {}),
        token.clone(),
    );
    session.set_query(query);
    let deadline = Instant::now() + budget;
    let mut timed_out = false;
    // Settled = two ticks in a row with nothing left to do: the first may have run before the
    // walker injected its last paths, the second runs after the walk was seen to be over.
    let mut quiet_ticks = 0;
    loop {
        let status = session.tick(10);
        if cancel.is_cancelled() {
            return Outcome::error("cancelled".to_owned());
        }
        quiet_ticks = if status.busy() || status.changed {
            0
        } else {
            quiet_ticks + 1
        };
        if quiet_ticks >= 2 {
            break;
        }
        if Instant::now() >= deadline {
            timed_out = status.walking;
            break;
        }
    }
    let matched = usize::try_from(session.matched_count()).unwrap_or(usize::MAX);
    let paths = session
        .results(0, u32::try_from(limit).unwrap_or(u32::MAX))
        .into_iter()
        .map(|r| display(&r.path))
        .collect();
    token.cancel();
    Outcome::reply(ClaudeResponse::Files {
        paths,
        truncated: matched > limit || timed_out,
    })
}

/// `search_contents`: the content search pipeline, collected into one answer.
pub fn search_contents(
    root: &Path,
    pattern: &str,
    glob: Option<&str>,
    regex: bool,
    limit: usize,
    ctx: &IpcContext,
    cancel: &CancellationToken,
) -> Outcome {
    search_contents_with_budget(
        root,
        pattern,
        glob,
        regex,
        limit,
        ctx,
        cancel,
        CONTENT_SEARCH_BUDGET,
    )
}

/// [`search_contents`] with an explicit wall-clock budget (tests widen or shrink it).
#[allow(clippy::too_many_arguments)]
fn search_contents_with_budget(
    root: &Path,
    pattern: &str,
    glob: Option<&str>,
    regex: bool,
    limit: usize,
    ctx: &IpcContext,
    cancel: &CancellationToken,
    budget: Duration,
) -> Outcome {
    if pattern.is_empty() {
        return Outcome::error("empty pattern".to_owned());
    }
    if !root.is_dir() {
        return Outcome::error(format!("{} is not a directory", root.display()));
    }
    let limit = limit.clamp(1, MAX_CONTENT_LIMIT);
    let mut query = GrepQuery::new(root, pattern);
    query.regex = regex;
    query.glob = glob.unwrap_or_default().to_owned();
    query.show_hidden = ctx.show_hidden;
    query.respect_gitignore = ctx.respect_gitignore;
    query.max_matches = limit;

    let token = cancel.child_token();
    let (tx, rx) = mpsc::channel();
    let sink: ks_search::EventSink<GrepEvent> = Arc::new(move |event| {
        let _ = tx.send(event);
    });
    let handle = match spawn_search(query, 1, token.clone(), sink) {
        Ok(handle) => handle,
        Err(e) => return Outcome::error(format!("cannot start the search: {e}")),
    };

    let deadline = Instant::now() + budget;
    let mut files = Vec::new();
    let mut truncated = false;
    loop {
        if cancel.is_cancelled() || Instant::now() >= deadline {
            token.cancel();
            truncated = true;
            break;
        }
        match rx.recv_timeout(Duration::from_millis(25)) {
            Ok(GrepEvent::Matches { files: batch, .. }) => files.extend(batch),
            Ok(GrepEvent::Done { capped, .. }) => {
                truncated = capped;
                break;
            }
            Ok(GrepEvent::Cancelled { .. }) => {
                truncated = true;
                break;
            }
            Ok(GrepEvent::Error { message, .. }) => {
                return Outcome::error(format!("invalid search: {message}"));
            }
            Ok(GrepEvent::Progress { .. }) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    drop(handle);

    // Files arrive in no particular order; give the answer a stable one.
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let mut hits = Vec::new();
    'files: for file in files {
        for line in file.lines {
            if hits.len() == limit {
                truncated = true;
                break 'files;
            }
            hits.push(ClaudeHit {
                path: display(&file.path),
                line: usize::try_from(line.line_number).unwrap_or(usize::MAX),
                text: line.text,
            });
        }
    }
    Outcome::reply(ClaudeResponse::Matches { hits, truncated })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    /// Generous budget: only an upper bound (a search ends as soon as it settles), so a
    /// starved machine cannot turn a tiny tree into a "timed out" answer.
    const TEST_BUDGET: Duration = Duration::from_secs(120);

    fn search_files(
        root: &Path,
        query: &str,
        limit: usize,
        ctx: &IpcContext,
        cache: &Arc<WalkCache>,
        cancel: &CancellationToken,
    ) -> Outcome {
        search_files_with_budget(root, query, limit, ctx, cache, cancel, TEST_BUDGET)
    }

    fn search_contents(
        root: &Path,
        pattern: &str,
        glob: Option<&str>,
        regex: bool,
        limit: usize,
        ctx: &IpcContext,
        cancel: &CancellationToken,
    ) -> Outcome {
        search_contents_with_budget(root, pattern, glob, regex, limit, ctx, cancel, TEST_BUDGET)
    }

    #[test]
    fn an_exhausted_budget_reports_truncation() {
        let dir = tree();
        let c = ctx(dir.path());
        let out = search_files_with_budget(
            dir.path(),
            "",
            50,
            &c,
            &Arc::new(WalkCache::new()),
            &CancellationToken::new(),
            Duration::ZERO,
        );
        let (_, truncated) = files_of(out);
        assert!(truncated);
        let out = search_contents_with_budget(
            dir.path(),
            "hello",
            None,
            false,
            50,
            &c,
            &CancellationToken::new(),
            Duration::ZERO,
        );
        let (_, truncated) = matches_of(out);
        assert!(truncated);
    }

    fn ctx(root: &Path) -> IpcContext {
        IpcContext {
            root: root.to_path_buf(),
            cwd: root.to_path_buf(),
            show_hidden: false,
            respect_gitignore: true,
        }
    }

    fn tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("src/deep")).unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(root.join("ignored_dir")).unwrap();
        fs::write(root.join(".gitignore"), "ignored_dir/\n").unwrap();
        fs::write(root.join("README.md"), "# Title\nhello World\n").unwrap();
        fs::write(
            root.join("src/main.rs"),
            "fn main() {\n    println!(\"hello\");\n}\n",
        )
        .unwrap();
        fs::write(root.join("src/deep/mod.rs"), "pub fn hello_world() {}\n").unwrap();
        fs::write(root.join(".git/config"), "hello\n").unwrap();
        fs::write(root.join("ignored_dir/skip.txt"), "hello\n").unwrap();
        fs::write(root.join("blob.bin"), b"hello\0binary").unwrap();
        dir
    }

    fn files_of(outcome: Outcome) -> (Vec<String>, bool) {
        match outcome.response {
            ClaudeResponse::Files { paths, truncated } => (paths, truncated),
            other => panic!("{other:?}"),
        }
    }

    fn matches_of(outcome: Outcome) -> (Vec<ClaudeHit>, bool) {
        match outcome.response {
            ClaudeResponse::Matches { hits, truncated } => (hits, truncated),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn list_dir_sorts_directories_first_and_reports_sizes() {
        let dir = tree();
        let out = list_dir(dir.path(), &CancellationToken::new());
        let ClaudeResponse::Dir { entries } = out.response else {
            panic!("{out:?}")
        };
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            [
                ".git",
                "ignored_dir",
                "src",
                ".gitignore",
                "blob.bin",
                "README.md"
            ]
        );
        let readme = entries.iter().find(|e| e.name == "README.md").unwrap();
        assert_eq!(readme.size, Some(20));
        assert!(entries
            .iter()
            .find(|e| e.name == "src")
            .unwrap()
            .size
            .is_none());
        assert!(matches!(
            list_dir(&dir.path().join("missing"), &CancellationToken::new()).response,
            ClaudeResponse::Error { .. }
        ));
    }

    #[test]
    fn list_dir_says_when_it_truncates() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..(MAX_LIST_ENTRIES + 50) {
            std::fs::write(dir.path().join(format!("f{i:05}.txt")), "").unwrap();
        }
        let out = list_dir(dir.path(), &CancellationToken::new());
        let ClaudeResponse::Dir { entries } = out.response else {
            panic!("{out:?}")
        };
        assert_eq!(entries.len(), MAX_LIST_ENTRIES + 1);
        assert_eq!(entries[0].name, "f00000.txt");
        let last = entries.last().unwrap();
        assert!(last.name.contains("truncated"), "{}", last.name);
        assert!(last.name.contains("2050"), "{}", last.name);
    }

    #[test]
    fn reveal_checks_existence_and_names_the_path() {
        let dir = tree();
        let ok = reveal(&dir.path().join("src/main.rs"));
        assert_eq!(ok.response, ClaudeResponse::Ok);
        assert_eq!(ok.reveal, Some(dir.path().join("src/main.rs")));
        let missing = reveal(&dir.path().join("nope"));
        assert!(matches!(missing.response, ClaudeResponse::Error { .. }));
        assert!(missing.reveal.is_none());
    }

    #[test]
    fn search_files_uses_the_fuzzy_finder_and_respects_limits_and_ignores() {
        let dir = tree();
        let cache = Arc::new(WalkCache::new());
        let c = ctx(dir.path());
        let (paths, truncated) = files_of(search_files(
            dir.path(),
            "main",
            10,
            &c,
            &cache,
            &CancellationToken::new(),
        ));
        assert!(!truncated);
        assert_eq!(paths.len(), 1);
        assert!(paths[0].ends_with("main.rs"), "{paths:?}");

        // Fuzzy: a subsequence of the path matches, ignored/.git content never shows.
        let (paths, _) = files_of(search_files(
            dir.path(),
            "dmr",
            10,
            &c,
            &cache,
            &CancellationToken::new(),
        ));
        assert!(paths.iter().any(|p| p.ends_with("mod.rs")), "{paths:?}");
        let (all, _) = files_of(search_files(
            dir.path(),
            "",
            50,
            &c,
            &cache,
            &CancellationToken::new(),
        ));
        assert!(all
            .iter()
            .all(|p| !p.contains("ignored_dir") && !p.contains(".git")));

        // The limit truncates and says so.
        let (some, truncated) = files_of(search_files(
            dir.path(),
            "",
            2,
            &c,
            &cache,
            &CancellationToken::new(),
        ));
        assert_eq!(some.len(), 2);
        assert!(truncated);

        // A second search over the same root is served from the walk cache.
        let (again, _) = files_of(search_files(
            dir.path(),
            "main",
            10,
            &c,
            &cache,
            &CancellationToken::new(),
        ));
        assert!(again[0].ends_with("main.rs"), "{again:?}");

        assert!(matches!(
            search_files(
                &dir.path().join("missing"),
                "x",
                1,
                &c,
                &cache,
                &CancellationToken::new()
            )
            .response,
            ClaudeResponse::Error { .. }
        ));
    }

    #[test]
    fn search_contents_literal_regex_glob_limit_and_errors() {
        let dir = tree();
        let root = dir.path();
        let c = ctx(root);
        let none = CancellationToken::new();

        // Smart case: lower case matches README's "hello World" too. Binary, .git and ignored
        // directories are skipped.
        let (hits, truncated) =
            matches_of(search_contents(root, "hello", None, false, 50, &c, &none));
        let mut files: Vec<String> = hits
            .iter()
            .map(|h| {
                Path::new(&h.path)
                    .strip_prefix(dunce::simplified(root))
                    .unwrap_or(Path::new(&h.path))
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        files.sort();
        assert!(!truncated);
        assert_eq!(
            files,
            ["README.md", "src/deep/mod.rs", "src/main.rs"],
            "{hits:?}"
        );

        let (hits, _) = matches_of(search_contents(
            root,
            r"fn \w+\(",
            Some("*.rs"),
            true,
            50,
            &c,
            &none,
        ));
        assert_eq!(hits.len(), 2, "{hits:?}");
        assert!(hits
            .iter()
            .any(|h| h.line == 1 && h.text.starts_with("fn main")));

        let (hits, truncated) =
            matches_of(search_contents(root, "hello", None, false, 1, &c, &none));
        assert_eq!(hits.len(), 1);
        assert!(truncated);

        for (pattern, regex) in [("(", true), ("", false)] {
            assert!(
                matches!(
                    search_contents(root, pattern, None, regex, 5, &c, &none).response,
                    ClaudeResponse::Error { .. }
                ),
                "{pattern}"
            );
        }
    }

    #[test]
    fn cancelled_requests_stop_early() {
        let dir = tree();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let out = search_files(
            dir.path(),
            "main",
            5,
            &ctx(dir.path()),
            &Arc::new(WalkCache::new()),
            &cancel,
        );
        assert!(matches!(out.response, ClaudeResponse::Error { .. }));
    }

    #[test]
    fn relative_paths_resolve_against_the_project_root() {
        let root = Path::new("/proj");
        assert_eq!(resolve(root, "a/b"), root.join("a/b"));
        let abs = std::env::temp_dir();
        assert_eq!(resolve(root, abs.to_str().unwrap()), abs);
    }
}
