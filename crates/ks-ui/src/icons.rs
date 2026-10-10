//! Icons for the three icon sets of PLAN §4.5, chosen by special name, then extension, then
//! kind. Every icon is a single terminal cell wide.

use ks_core::config::IconSet;
use ks_core::{Entry, EntryKind};

/// Nerd Font glyphs (code points of the Font Awesome, Devicons and Seti ranges).
mod nerd {
    pub const FOLDER: &str = "\u{f07b}";
    pub const FOLDER_GIT: &str = "\u{e5fb}";
    pub const FOLDER_NPM: &str = "\u{e5fa}";
    pub const FOLDER_CONFIG: &str = "\u{e5fc}";
    pub const FILE: &str = "\u{f15b}";
    pub const TEXT: &str = "\u{f15c}";
    pub const LINK: &str = "\u{f0c1}";
    pub const CLOUD: &str = "\u{f0c2}";
    pub const EXEC: &str = "\u{f013}";
    pub const TERMINAL: &str = "\u{f120}";
    pub const WINDOWS: &str = "\u{e70f}";
    pub const CODE: &str = "\u{f1c9}";
    pub const IMAGE: &str = "\u{f1c5}";
    pub const ARCHIVE: &str = "\u{f1c6}";
    pub const AUDIO: &str = "\u{f1c7}";
    pub const VIDEO: &str = "\u{f1c8}";
    pub const PDF: &str = "\u{f1c1}";
    pub const WORD: &str = "\u{f1c2}";
    pub const EXCEL: &str = "\u{f1c3}";
    pub const SLIDES: &str = "\u{f1c4}";
    pub const RUST: &str = "\u{e7a8}";
    pub const PYTHON: &str = "\u{e73c}";
    pub const JS: &str = "\u{e74e}";
    pub const TS: &str = "\u{e628}";
    pub const JSON: &str = "\u{e60b}";
    pub const MARKDOWN: &str = "\u{e73e}";
    pub const HTML: &str = "\u{e736}";
    pub const CSS: &str = "\u{e749}";
    pub const C: &str = "\u{e61e}";
    pub const CPP: &str = "\u{e61d}";
    pub const GO: &str = "\u{e627}";
    pub const JAVA: &str = "\u{e738}";
    pub const CONFIG: &str = "\u{e615}";
    pub const LOCK: &str = "\u{f023}";
    pub const GIT: &str = "\u{e702}";
    pub const DOCKER: &str = "\u{f308}";
    pub const BOOK: &str = "\u{f02d}";
    pub const KEY: &str = "\u{f084}";
    pub const DATABASE: &str = "\u{f1c0}";
}

/// The icon for `entry` in `set`. Cloud placeholders get a cloud so they are recognisable
/// before anything tries to open them.
pub fn icon_for(entry: &Entry, set: IconSet) -> &'static str {
    match set {
        IconSet::Nerd => nerd_icon(entry),
        IconSet::Unicode => unicode_icon(entry),
        IconSet::Ascii => ascii_icon(entry),
    }
}

fn nerd_icon(entry: &Entry) -> &'static str {
    if entry.is_cloud_placeholder() {
        return nerd::CLOUD;
    }
    let name = entry.display_name().to_lowercase();
    if entry.is_dir {
        return match name.as_str() {
            ".git" => nerd::FOLDER_GIT,
            "node_modules" => nerd::FOLDER_NPM,
            ".config" | ".vscode" | ".github" => nerd::FOLDER_CONFIG,
            _ => nerd::FOLDER,
        };
    }
    if entry.is_link() {
        return nerd::LINK;
    }
    if let Some(icon) = by_special_name(&name) {
        return icon;
    }
    if let Some(icon) = by_extension(entry.extension()) {
        return icon;
    }
    if entry.is_exec() {
        nerd::EXEC
    } else {
        nerd::FILE
    }
}

fn by_special_name(name: &str) -> Option<&'static str> {
    Some(match name {
        "cargo.toml" | "cargo.lock" => nerd::RUST,
        "readme" | "readme.md" | "readme.txt" | "license" | "license.md" | "license.txt" => {
            nerd::BOOK
        }
        "dockerfile" | "docker-compose.yml" | "docker-compose.yaml" | ".dockerignore" => {
            nerd::DOCKER
        }
        ".gitignore" | ".gitattributes" | ".gitmodules" => nerd::GIT,
        "makefile" | "cmakelists.txt" => nerd::CONFIG,
        "package.json" | "package-lock.json" | "tsconfig.json" => nerd::JSON,
        _ => return None,
    })
}

fn by_extension(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "rs" => nerd::RUST,
        "py" | "pyw" | "ipynb" => nerd::PYTHON,
        "js" | "mjs" | "cjs" | "jsx" => nerd::JS,
        "ts" | "tsx" => nerd::TS,
        "json" | "jsonc" | "json5" => nerd::JSON,
        "md" | "markdown" | "mdx" => nerd::MARKDOWN,
        "html" | "htm" => nerd::HTML,
        "css" | "scss" | "sass" | "less" => nerd::CSS,
        "c" | "h" => nerd::C,
        "cpp" | "cc" | "cxx" | "hpp" | "hh" => nerd::CPP,
        "go" => nerd::GO,
        "java" | "kt" | "kts" | "cs" => nerd::JAVA,
        "toml" | "yaml" | "yml" | "ini" | "cfg" | "conf" | "env" => nerd::CONFIG,
        "lock" => nerd::LOCK,
        "txt" | "log" | "rtf" => nerd::TEXT,
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "svg" | "ico" | "tif" | "tiff" => {
            nerd::IMAGE
        }
        "zip" | "7z" | "rar" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" => nerd::ARCHIVE,
        "mp3" | "wav" | "flac" | "ogg" | "m4a" | "opus" => nerd::AUDIO,
        "mp4" | "mkv" | "avi" | "mov" | "webm" => nerd::VIDEO,
        "pdf" => nerd::PDF,
        "doc" | "docx" | "odt" => nerd::WORD,
        "xls" | "xlsx" | "ods" | "csv" => nerd::EXCEL,
        "ppt" | "pptx" | "odp" => nerd::SLIDES,
        "sh" | "bash" | "zsh" | "fish" | "ps1" | "psm1" | "bat" | "cmd" => nerd::TERMINAL,
        "exe" | "msi" | "dll" => nerd::WINDOWS,
        "pem" | "key" | "pub" | "crt" | "cer" => nerd::KEY,
        "db" | "sqlite" | "sqlite3" | "sql" => nerd::DATABASE,
        "lua" | "rb" | "php" | "swift" | "zig" | "hs" | "ml" | "ex" | "exs" | "dart" | "vue"
        | "svelte" => nerd::CODE,
        _ => return None,
    })
}

fn unicode_icon(entry: &Entry) -> &'static str {
    if entry.is_cloud_placeholder() {
        "☁"
    } else if entry.is_dir && !entry.is_link() {
        "▸"
    } else if entry.is_link() {
        "↪"
    } else if entry.is_exec() {
        "»"
    } else {
        "·"
    }
}

fn ascii_icon(entry: &Entry) -> &'static str {
    if entry.is_cloud_placeholder() {
        "~"
    } else if entry.kind == EntryKind::Symlink || entry.kind == EntryKind::Junction {
        "@"
    } else if entry.is_dir {
        "+"
    } else if entry.is_exec() {
        "*"
    } else {
        "-"
    }
}

#[cfg(test)]
mod tests {
    use ks_core::Attrs;
    use unicode_width::UnicodeWidthStr;

    use super::*;

    fn file(name: &str) -> Entry {
        Entry::file(name, 1)
    }

    #[test]
    fn nerd_icons_by_name_extension_and_kind() {
        assert_eq!(icon_for(&file("main.rs"), IconSet::Nerd), nerd::RUST);
        assert_eq!(icon_for(&file("MAIN.RS"), IconSet::Nerd), nerd::RUST);
        assert_eq!(icon_for(&file("Cargo.toml"), IconSet::Nerd), nerd::RUST);
        assert_eq!(icon_for(&file("README.md"), IconSet::Nerd), nerd::BOOK);
        assert_eq!(icon_for(&file("notes.md"), IconSet::Nerd), nerd::MARKDOWN);
        assert_eq!(icon_for(&file("pic.PNG"), IconSet::Nerd), nerd::IMAGE);
        assert_eq!(icon_for(&file("unknown.zzz"), IconSet::Nerd), nerd::FILE);
        assert_eq!(icon_for(&file("noext"), IconSet::Nerd), nerd::FILE);
        assert_eq!(icon_for(&Entry::dir("src"), IconSet::Nerd), nerd::FOLDER);
        assert_eq!(
            icon_for(&Entry::dir(".git"), IconSet::Nerd),
            nerd::FOLDER_GIT
        );
        assert_eq!(
            icon_for(&Entry::dir("node_modules"), IconSet::Nerd),
            nerd::FOLDER_NPM
        );
        assert_eq!(
            icon_for(&Entry::new("l", EntryKind::Symlink), IconSet::Nerd),
            nerd::LINK
        );
        let exec = file("tool").with_attrs(Attrs::EXEC);
        assert_eq!(icon_for(&exec, IconSet::Nerd), nerd::EXEC);
    }

    #[test]
    fn cloud_placeholders_always_show_a_cloud() {
        let cloud = file("photo.jpg").with_attrs(Attrs::CLOUD);
        assert_eq!(icon_for(&cloud, IconSet::Nerd), nerd::CLOUD);
        assert_eq!(icon_for(&cloud, IconSet::Unicode), "☁");
        assert_eq!(icon_for(&cloud, IconSet::Ascii), "~");
        let dir = Entry::dir("OneDrive").with_attrs(Attrs::CLOUD);
        assert_eq!(icon_for(&dir, IconSet::Nerd), nerd::CLOUD);
    }

    #[test]
    fn unicode_and_ascii_sets_distinguish_the_kinds() {
        let link = Entry::new("l", EntryKind::Symlink);
        let junction = Entry::new("j", EntryKind::Junction);
        let exec = file("x").with_attrs(Attrs::EXEC);
        for set in [IconSet::Unicode, IconSet::Ascii] {
            let icons: Vec<_> = [Entry::dir("d"), file("f"), link.clone(), exec.clone()]
                .iter()
                .map(|e| icon_for(e, set))
                .collect();
            let unique: std::collections::HashSet<_> = icons.iter().collect();
            assert_eq!(unique.len(), icons.len(), "{set:?}: {icons:?}");
        }
        assert_eq!(icon_for(&junction, IconSet::Ascii), "@");
        assert!(icon_for(&Entry::dir("d"), IconSet::Ascii).is_ascii());
    }

    #[test]
    fn every_icon_is_one_cell_wide() {
        let samples = [
            "a.rs",
            "a.py",
            "a.js",
            "a.ts",
            "a.json",
            "a.md",
            "a.html",
            "a.css",
            "a.c",
            "a.cpp",
            "a.go",
            "a.java",
            "a.toml",
            "a.lock",
            "a.txt",
            "a.png",
            "a.zip",
            "a.mp3",
            "a.mp4",
            "a.pdf",
            "a.docx",
            "a.xlsx",
            "a.pptx",
            "a.sh",
            "a.exe",
            "a.pem",
            "a.db",
            "a.lua",
            "Dockerfile",
            ".gitignore",
            "README",
            "Makefile",
            "x",
        ];
        for set in [IconSet::Nerd, IconSet::Unicode, IconSet::Ascii] {
            for name in samples {
                let icon = icon_for(&file(name), set);
                assert_eq!(icon.width(), 1, "{set:?} {name}: {icon:?}");
            }
            for entry in [
                Entry::dir("d"),
                Entry::dir(".git"),
                Entry::new("l", EntryKind::Symlink),
                file("c").with_attrs(Attrs::CLOUD),
            ] {
                assert_eq!(icon_for(&entry, set).width(), 1, "{set:?}");
            }
        }
    }
}
