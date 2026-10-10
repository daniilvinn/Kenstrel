//! Generated documentation of the key bindings (`docs/keymap.md`).
//!
//! The text comes from the default keymap and the command registry, so it cannot drift from
//! the program: a test regenerates it and fails when the committed file differs.

use std::fmt::Write as _;

use crate::command::{self, Category};
use crate::help::keys_label;
use crate::keymap::{Binding, Keymap};
use crate::mode::KeyContext;

const INTRO: &str = "\
# Keymap

> Generated from the built-in keymap and command registry. Do not edit by hand:
> run `cargo run -p ks-app -- --dump-keymap > docs/keymap.md` (or
> `KESTREL_UPDATE_DOCS=1 cargo test -p ks-core docs`) after changing a default binding.

Every command is also available in the command palette (`:`) and listed in the help
overlay (`?`), both of which show your own bindings after rebinding.

## Rebinding

Add `[keymap.<context>]` tables to `config.toml`. The key is a space-separated key
sequence, the value a command id from the tables below:

```toml
[keymap.normal]
\"ctrl-t\" = \"tab.new\"          # add a binding
\"d\" = \"file.delete\"           # rebind an existing key
\"q\" = \"none\"                  # unbind
\"m <char>\" = \"bookmark.set\"   # <char> passes the next typed key to the command
```

Contexts: `normal`, `visual` (inherits `normal`), `prompt`, `filter`, `finder`, `grep`,
`palette`, `help`, `confirm`, `jobs`, `claude`. Binding a prefix (`g`) removes the longer
defaults that start with it (`g g`, `g h`, ...); the status line tells you when that
happens. Key names: single characters, `space`, `enter`, `esc`, `tab`, `backspace`,
`delete`, `up`, `down`, `left`, `right`, `home`, `end`, `pageup`, `pagedown`, `f1`..`f12`,
with `ctrl-`, `alt-` prefixes (`ctrl-p`, `alt-.`). Reload the file without restarting from
the palette (`Reload config`).

";

fn cell(text: &str) -> String {
    if text.is_empty() {
        "—".to_owned()
    } else {
        text.split(", ")
            .map(|k| format!("`{k}`"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// The complete `docs/keymap.md` for `keymap`.
pub fn keymap_markdown(keymap: &Keymap) -> String {
    let mut out = String::from(INTRO);
    out.push_str("## Normal mode\n\n");
    for &category in Category::ALL {
        let _ = writeln!(out, "### {}\n", category.title());
        out.push_str("| Keys | Command | Id | What it does |\n|---|---|---|---|\n");
        for info in command::all().filter(|i| i.category == category) {
            let bindings: Vec<&Binding> = keymap
                .bindings(KeyContext::Normal)
                .iter()
                .filter(|b| b.action == info.action)
                .collect();
            let _ = writeln!(
                out,
                "| {} | {} | `{}` | {} |",
                cell(&keys_label(&bindings)),
                info.title,
                info.id,
                info.description
            );
        }
        out.push('\n');
    }
    out.push_str("## Other contexts\n\n");
    for &ctx in KeyContext::ALL.iter().filter(|c| **c != KeyContext::Normal) {
        let bindings = keymap.bindings(ctx);
        if bindings.is_empty() {
            continue;
        }
        let note = match ctx.fallback() {
            Some(parent) => format!(" (also uses every `{parent}` binding it does not override)"),
            None => String::new(),
        };
        let _ = writeln!(out, "### `{ctx}`{note}\n");
        out.push_str("| Keys | Command |\n|---|---|\n");
        for binding in bindings {
            let target = match binding.arg {
                Some(arg) => format!("`{}:{arg}`", binding.action.id()),
                None => format!("`{}`", binding.action.id()),
            };
            let _ = writeln!(out, "| `{}` | {target} |", binding.keys_display());
        }
        out.push('\n');
    }
    out.truncate(out.trim_end().len());
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn committed_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/keymap.md")
    }

    #[test]
    fn docs_keymap_md_is_up_to_date() {
        let generated = keymap_markdown(&Keymap::defaults());
        let path = committed_path();
        if std::env::var_os("KESTREL_UPDATE_DOCS").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &generated).unwrap();
            return;
        }
        let committed = std::fs::read_to_string(&path)
            .unwrap_or_default()
            // Git may check the file out with CRLF line endings on Windows.
            .replace("\r\n", "\n");
        assert!(
            committed == generated,
            "docs/keymap.md is out of date; regenerate it with \
             `KESTREL_UPDATE_DOCS=1 cargo test -p ks-core docs`"
        );
    }

    #[test]
    fn every_command_and_context_appears() {
        let text = keymap_markdown(&Keymap::defaults());
        for info in command::all() {
            assert!(text.contains(&format!("`{}`", info.id)), "{}", info.id);
        }
        assert!(text.contains("### `palette`"));
        assert!(text.contains("| `1-9` |"), "digit bindings collapse");
        assert!(text.ends_with("|\n"));
    }

    #[test]
    fn rebinding_changes_the_text() {
        let mut map = Keymap::defaults();
        map.apply_overrides(
            &[(
                "normal".to_owned(),
                [("T".to_owned(), "file.trash".to_owned())].into(),
            )]
            .into(),
        );
        assert_ne!(keymap_markdown(&map), keymap_markdown(&Keymap::defaults()));
    }
}
