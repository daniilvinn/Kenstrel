//! A fake file system plus a minimal effect runner for the tests of tabs, bookmarks and
//! frecency (the navigation tests in `nav` have their own, private copy).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::aids::StateFile;
use crate::config::Config;
use crate::effect::Effect;
use crate::entry::Entry;
use crate::error::{FsError, FsErrorKind};
use crate::key::KeyChord;
use crate::keymap::Keymap;
use crate::msg::Msg;
use crate::state::{AppState, Places, Size, StartParams};
use crate::update::update;

pub struct World {
    pub state: AppState,
    pub dirs: HashMap<PathBuf, Vec<Entry>>,
    /// Every effect `update` returned, in order.
    pub log: Vec<Effect>,
    /// Results of effects, held back instead of delivered while `hold` is set.
    pub held: Vec<Msg>,
    pub hold: bool,
}

pub fn p(s: &str) -> PathBuf {
    PathBuf::from(s)
}

impl World {
    pub fn new(cwd: &str) -> Self {
        let state = AppState::new(StartParams {
            cwd: p(cwd),
            home: Some(p("/home/me")),
            places: Places {
                home: Some(p("/home/me")),
                ..Places::default()
            },
            config: Config::default(),
            keymap: Keymap::defaults(),
            size: Size::new(80, 24),
            now: SystemTime::UNIX_EPOCH,
            utc_offset_secs: 0,
        });
        let mut dirs = HashMap::new();
        dirs.insert(p("/"), vec![Entry::dir("home")]);
        Self {
            state,
            dirs,
            log: Vec::new(),
            held: Vec::new(),
            hold: false,
        }
    }

    pub fn dir(&mut self, path: &str, entries: Vec<Entry>) -> &mut Self {
        self.dirs.insert(p(path), entries);
        self
    }

    pub fn send(&mut self, msg: Msg) {
        let mut queue = vec![msg];
        while let Some(msg) = queue.pop() {
            let effects = update(&mut self.state, msg);
            for effect in &effects {
                let results = self.run(effect);
                if self.hold {
                    self.held.extend(results);
                } else {
                    queue.extend(results.into_iter().rev());
                }
            }
            self.log.extend(effects);
        }
    }

    fn run(&self, effect: &Effect) -> Vec<Msg> {
        match effect {
            Effect::ListDir { gen, path } => {
                let gen = *gen;
                let Some(entries) = self.dirs.get(path) else {
                    return vec![Msg::ListingFinished {
                        gen,
                        error: Some(FsError::new(FsErrorKind::NotFound, "not found")),
                    }];
                };
                vec![
                    Msg::ListingChunk {
                        gen,
                        entries: entries.clone(),
                    },
                    Msg::ListingFinished { gen, error: None },
                ]
            }
            Effect::ListDrives { gen } => vec![Msg::DrivesLoaded {
                gen: *gen,
                drives: vec![crate::drives::DriveInfo::new("/")],
            }],
            _ => Vec::new(),
        }
    }

    pub fn key(&mut self, text: &str) {
        for part in text.split(' ') {
            self.send(Msg::Key(part.parse::<KeyChord>().unwrap()));
        }
    }

    pub fn start(&mut self) {
        self.send(Msg::Start);
    }

    pub fn names(&self) -> Vec<String> {
        (0..self.state.view.len())
            .map(|i| self.state.view.row(i).unwrap().display_name().into_owned())
            .collect()
    }

    pub fn cursor(&self) -> String {
        self.state
            .view
            .current()
            .map(|e| e.display_name().into_owned())
            .unwrap_or_default()
    }

    pub fn status(&self) -> String {
        self.state
            .status
            .as_ref()
            .map(|s| s.text.clone())
            .unwrap_or_default()
    }

    pub fn cwd(&self) -> &Path {
        &self.state.cwd
    }

    /// The text of the most recent write of `file`.
    pub fn last_write(&self, file: StateFile) -> Option<String> {
        self.log.iter().rev().find_map(|e| match e {
            Effect::WriteState { file: f, contents } if *f == file => Some(contents.clone()),
            _ => None,
        })
    }

    pub fn count(&self, pred: impl Fn(&Effect) -> bool) -> usize {
        self.log.iter().filter(|e| pred(e)).count()
    }
}

/// `/home/me` with a few project directories.
pub fn project_world() -> World {
    let mut w = World::new("/home/me");
    w.dir(
        "/home/me",
        vec![
            Entry::dir("projects"),
            Entry::dir("Downloads"),
            Entry::file("notes.txt", 10),
        ],
    );
    w.dir(
        "/home/me/projects",
        vec![
            Entry::dir("alpha"),
            Entry::dir("beta"),
            Entry::file("todo.md", 4),
        ],
    );
    w.dir("/home/me/projects/alpha", vec![Entry::file("main.rs", 100)]);
    w.dir("/home/me/projects/beta", vec![]);
    w.dir("/home/me/Downloads", vec![Entry::file("setup.exe", 5000)]);
    w.start();
    w.state.aids.mark_loaded();
    w
}
