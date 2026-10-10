//! "Open with" handlers registered for a file extension (Windows registry).
//!
//! Sources, in order (earlier wins on duplicates):
//!
//! 1. `HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.ext\OpenWithList`
//!    (program names, most recently used first per `MRUList`) and `...\OpenWithProgids`;
//! 2. `HKCR\.ext\OpenWithList` (sub keys named after programs) and `HKCR\.ext\OpenWithProgids`
//!    (value names are ProgIds).
//!
//! A program name resolves through `HKCR\Applications\<exe>\shell\open\command`, then through
//! `App Paths`, then `PATH`; a ProgId through `HKCR\<progid>\shell\open\command`. The label is
//! the `FriendlyAppName` when there is a plain-text one, otherwise the program's file stem.
//!
//! The parsing helpers are portable and unit-tested; reading the registry is Windows-only and
//! blocks on registry IO, so call it off the UI thread.

use super::editor::{split_command_line, CommandSpec};
use super::open::OpenHandler;

/// Lowercase extension of `path` with its leading dot (`.txt`), if it has one.
pub fn dotted_extension(path: &std::path::Path) -> Option<String> {
    let ext = path.extension()?.to_string_lossy().to_lowercase();
    (!ext.is_empty()).then(|| format!(".{ext}"))
}

/// Turn a registry shell command (`"C:\x\y.exe" /open "%1"`) into a program and argument
/// templates with `{file}` for the placeholder. Environment variables like `%SystemRoot%` in
/// the program are expanded through `lookup`. A command without a file placeholder gets
/// `{file}` appended. `None` for an empty command.
pub fn command_to_spec(
    command: &str,
    lookup: impl Fn(&str) -> Option<String>,
) -> Option<CommandSpec> {
    let mut parts = split_command_line(command.trim()).into_iter();
    let program = expand_env(&parts.next()?, &lookup);
    if program.is_empty() {
        return None;
    }
    let mut has_file = false;
    let mut args = Vec::new();
    for arg in parts {
        let mut text = expand_env(&arg, &lookup);
        for token in ["%1", "%L", "%l", "%0"] {
            if text.contains(token) {
                text = text.replace(token, "{file}");
                has_file = true;
            }
        }
        let text = strip_extra_placeholders(&text);
        if !text.is_empty() {
            args.push(text);
        }
    }
    if !has_file {
        args.push("{file}".to_owned());
    }
    Some(CommandSpec { program, args })
}

/// Drop `%2`..`%9` and `%*`; they have no meaning for a single file.
fn strip_extra_placeholders(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%'
            && chars
                .peek()
                .is_some_and(|n| n.is_ascii_digit() || *n == '*')
        {
            chars.next();
        } else {
            out.push(c);
        }
    }
    out
}

/// Replace `%NAME%` using `lookup`; unknown names and `%1` style placeholders stay as they are.
pub fn expand_env(text: &str, lookup: &impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let value = after
            .find('%')
            .filter(|end| *end > 0)
            .and_then(|end| lookup(&after[..end]).map(|v| (end, v)));
        match value {
            Some((end, v)) => {
                out.push_str(&v);
                rest = &after[end + 1..];
            }
            None => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Order the program names of an `OpenWithList` key: the `MRUList` letters first (each letter
/// names the value `a`, `b`, ...), then the remaining values in their stored order.
/// `values` are `(value name, data)`; `MRUList` itself is excluded from the result.
pub fn mru_order(values: &[(String, String)]) -> Vec<String> {
    let mru = values
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("MRUList"))
        .map(|(_, data)| data.as_str())
        .unwrap_or("");
    let mut out: Vec<String> = Vec::new();
    let mut push = |data: &str| {
        if !data.is_empty() && !out.iter().any(|d| d.eq_ignore_ascii_case(data)) {
            out.push(data.to_owned());
        }
    };
    for letter in mru.chars() {
        if let Some((_, data)) = values
            .iter()
            .find(|(name, _)| name.chars().eq(std::iter::once(letter)))
        {
            push(data);
        }
    }
    for (name, data) in values {
        if !name.eq_ignore_ascii_case("MRUList") {
            push(data);
        }
    }
    out
}

/// A label from a registry display string; `@{...}` / `@dll,-id` resource references need the
/// shell to resolve and are rejected so the caller falls back to the file stem.
pub fn plain_label(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty() && !text.starts_with('@')).then(|| text.to_owned())
}

/// The label for a program path: its file stem with the first letter upper-cased.
pub fn stem_label(program: &str) -> String {
    let file = program.rsplit(['/', '\\']).next().unwrap_or(program);
    let stem = file.rsplit_once('.').map_or(file, |(stem, _)| stem);
    let mut chars = stem.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().collect::<String>() + chars.as_str())
        .unwrap_or_default()
}

/// Append `handler` unless one with the same label or the same program file is already listed.
pub fn push_unique(list: &mut Vec<OpenHandler>, handler: OpenHandler) {
    let file = |s: &str| {
        s.rsplit(['/', '\\'])
            .next()
            .unwrap_or(s)
            .to_ascii_lowercase()
    };
    let duplicate = list.iter().any(|h| {
        h.name.eq_ignore_ascii_case(&handler.name)
            || file(&h.command.program) == file(&handler.command.program)
    });
    if !duplicate {
        list.push(handler);
    }
}

/// Read access to the registry facts `collect` needs, so the merge and ordering logic runs
/// against a fake in tests.
pub trait RegistrySource {
    /// `(value name, program)` pairs of `HKCU\...\FileExts\<ext>\OpenWithList`.
    fn user_open_with_list(&self, ext: &str) -> Vec<(String, String)>;
    /// Value names of `HKCU\...\FileExts\<ext>\OpenWithProgids`.
    fn user_progids(&self, ext: &str) -> Vec<String>;
    /// Sub key names of `HKCR\<ext>\OpenWithList`.
    fn classes_open_with_list(&self, ext: &str) -> Vec<String>;
    /// Value names of `HKCR\<ext>\OpenWithProgids`.
    fn classes_progids(&self, ext: &str) -> Vec<String>;
    /// Default value of `HKCR\Applications\<exe>\shell\open\command`.
    fn application_command(&self, exe: &str) -> Option<String>;
    /// Default value of `App Paths\<exe>`.
    fn app_path(&self, exe: &str) -> Option<String>;
    /// `exe` found on `PATH`.
    fn find_in_path(&self, exe: &str) -> Option<String>;
    /// Default value of `HKCR\<progid>\shell\open\command`.
    fn progid_command(&self, progid: &str) -> Option<String>;
    /// `FriendlyAppName` of `HKCR\Applications\<exe>`.
    fn application_friendly_name(&self, exe: &str) -> Option<String>;
    /// `FriendlyAppName` of `HKCR\<progid>\shell\open`.
    fn progid_friendly_name(&self, progid: &str) -> Option<String>;
    /// Environment variable lookup for `%NAME%` expansion.
    fn env(&self, name: &str) -> Option<String>;
}

fn quoted(path: &str) -> String {
    format!("\"{}\"", path.trim_matches('"'))
}

fn from_application(src: &impl RegistrySource, exe: &str) -> Option<OpenHandler> {
    let lookup = |n: &str| src.env(n);
    let command = src
        .application_command(exe)
        .and_then(|c| command_to_spec(&c, lookup))
        .or_else(|| command_to_spec(&quoted(&src.app_path(exe)?), lookup))
        .or_else(|| command_to_spec(&quoted(&src.find_in_path(exe)?), lookup))?;
    let name = src
        .application_friendly_name(exe)
        .and_then(|t| plain_label(&t))
        .unwrap_or_else(|| stem_label(&command.program));
    Some(OpenHandler { name, command })
}

fn from_progid(src: &impl RegistrySource, progid: &str) -> Option<OpenHandler> {
    let command = src
        .progid_command(progid)
        .and_then(|c| command_to_spec(&c, |n| src.env(n)))?;
    let exe = command.program.rsplit(['/', '\\']).next().unwrap_or("");
    let name = src
        .application_friendly_name(exe)
        .and_then(|t| plain_label(&t))
        .or_else(|| {
            src.progid_friendly_name(progid)
                .and_then(|t| plain_label(&t))
        })
        .unwrap_or_else(|| stem_label(&command.program));
    Some(OpenHandler { name, command })
}

/// Merge the handlers `src` knows for `ext`: programs (HKCU most-recently-used first, then
/// HKCR), then ProgIds (HKCU, then HKCR), duplicates collapsed.
pub fn collect(src: &impl RegistrySource, ext: &str) -> Vec<OpenHandler> {
    let mut exes = mru_order(&src.user_open_with_list(ext));
    exes.extend(src.classes_open_with_list(ext));
    let mut progids = src.user_progids(ext);
    progids.extend(src.classes_progids(ext));
    let mut result = Vec::new();
    for handler in exes.iter().filter_map(|exe| from_application(src, exe)) {
        push_unique(&mut result, handler);
    }
    for handler in progids
        .iter()
        .filter(|p| !p.is_empty())
        .filter_map(|p| from_progid(src, p))
    {
        push_unique(&mut result, handler);
    }
    result
}

/// Handlers the registry lists for `ext` (with the leading dot, e.g. `.txt`). Empty off
/// Windows or when nothing is registered.
pub fn registry_handlers(ext: &str) -> Vec<OpenHandler> {
    #[cfg(windows)]
    {
        collect(&win::Real, ext)
    }
    #[cfg(not(windows))]
    {
        let _ = ext;
        Vec::new()
    }
}

#[cfg(windows)]
mod win {
    use winreg::enums::{HKEY_CLASSES_ROOT, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ};
    use winreg::{RegKey, HKEY};

    use super::RegistrySource;
    use crate::launch::editor::find_in_path;

    const EXPLORER: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts";
    const APP_PATHS: &str = r"Software\Microsoft\Windows\CurrentVersion\App Paths";

    pub(super) struct Real;

    fn open(root: HKEY, path: &str) -> Option<RegKey> {
        RegKey::predef(root)
            .open_subkey_with_flags(path, KEY_READ)
            .ok()
    }

    fn string_values(key: &RegKey) -> Vec<(String, String)> {
        key.enum_values()
            .flatten()
            .filter_map(|(name, _)| {
                let data: String = key.get_value(&name).ok()?;
                Some((name, data))
            })
            .collect()
    }

    fn value_names(key: &RegKey) -> Vec<String> {
        key.enum_values().flatten().map(|(name, _)| name).collect()
    }

    fn default_value(root: HKEY, path: &str) -> Option<String> {
        open(root, path)?.get_value::<String, _>("").ok()
    }

    fn friendly(root: HKEY, path: &str) -> Option<String> {
        open(root, path)?
            .get_value::<String, _>("FriendlyAppName")
            .ok()
    }

    impl RegistrySource for Real {
        fn user_open_with_list(&self, ext: &str) -> Vec<(String, String)> {
            open(
                HKEY_CURRENT_USER,
                &format!(r"{EXPLORER}\{ext}\OpenWithList"),
            )
            .map(|k| string_values(&k))
            .unwrap_or_default()
        }
        fn user_progids(&self, ext: &str) -> Vec<String> {
            open(
                HKEY_CURRENT_USER,
                &format!(r"{EXPLORER}\{ext}\OpenWithProgids"),
            )
            .map(|k| value_names(&k))
            .unwrap_or_default()
        }
        fn classes_open_with_list(&self, ext: &str) -> Vec<String> {
            open(HKEY_CLASSES_ROOT, &format!(r"{ext}\OpenWithList"))
                .map(|k| k.enum_keys().flatten().collect())
                .unwrap_or_default()
        }
        fn classes_progids(&self, ext: &str) -> Vec<String> {
            open(HKEY_CLASSES_ROOT, &format!(r"{ext}\OpenWithProgids"))
                .map(|k| value_names(&k))
                .unwrap_or_default()
        }
        fn application_command(&self, exe: &str) -> Option<String> {
            default_value(
                HKEY_CLASSES_ROOT,
                &format!(r"Applications\{exe}\shell\open\command"),
            )
        }
        fn app_path(&self, exe: &str) -> Option<String> {
            [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE]
                .into_iter()
                .find_map(|root| default_value(root, &format!(r"{APP_PATHS}\{exe}")))
        }
        fn find_in_path(&self, exe: &str) -> Option<String> {
            find_in_path(exe).map(|p| p.to_string_lossy().into_owned())
        }
        fn progid_command(&self, progid: &str) -> Option<String> {
            default_value(HKEY_CLASSES_ROOT, &format!(r"{progid}\shell\open\command"))
        }
        fn application_friendly_name(&self, exe: &str) -> Option<String> {
            friendly(HKEY_CLASSES_ROOT, &format!(r"Applications\{exe}"))
        }
        fn progid_friendly_name(&self, progid: &str) -> Option<String> {
            friendly(HKEY_CLASSES_ROOT, &format!(r"{progid}\shell\open"))
        }
        fn env(&self, name: &str) -> Option<String> {
            std::env::var(name).ok()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn env(name: &str) -> Option<String> {
        (name == "SystemRoot").then(|| r"C:\Windows".to_owned())
    }

    #[test]
    fn extension_is_lowercased_with_a_dot() {
        assert_eq!(
            dotted_extension(Path::new("a/B.TXT")).as_deref(),
            Some(".txt")
        );
        assert_eq!(dotted_extension(Path::new("Makefile")), None);
        assert_eq!(dotted_extension(Path::new(".bashrc")), None);
    }

    #[test]
    fn commands_become_templates() {
        let spec = command_to_spec(r#""C:\Program Files\App\app.exe" /open "%1""#, env).unwrap();
        assert_eq!(spec.program, r"C:\Program Files\App\app.exe");
        assert_eq!(spec.args, ["/open", "{file}"]);

        let spec = command_to_spec(r"%SystemRoot%\system32\NOTEPAD.EXE %1", env).unwrap();
        assert_eq!(spec.program, r"C:\Windows\system32\NOTEPAD.EXE");
        assert_eq!(spec.args, ["{file}"]);

        // placeholders other than the file are dropped; a missing one is appended
        let spec = command_to_spec(r#"app.exe --flag %2 "%*""#, env).unwrap();
        assert_eq!(spec.args, ["--flag", "{file}"]);
        let spec = command_to_spec(r#"app.exe --file="%L""#, env).unwrap();
        assert_eq!(spec.args, ["--file={file}"]);
        assert_eq!(command_to_spec("   ", env), None);
    }

    #[test]
    fn unknown_variables_are_left_alone() {
        assert_eq!(expand_env("%NOPE%\\x %1", &env), "%NOPE%\\x %1");
        assert_eq!(expand_env("100%", &env), "100%");
        assert_eq!(expand_env("%SystemRoot%", &env), r"C:\Windows");
    }

    #[test]
    fn mru_list_orders_values() {
        let pair = |a: &str, b: &str| (a.to_owned(), b.to_owned());
        let values = vec![
            pair("a", "notepad.exe"),
            pair("b", "Code.exe"),
            pair("c", "wordpad.exe"),
            pair("MRUList", "cab"),
        ];
        assert_eq!(
            mru_order(&values),
            ["wordpad.exe", "notepad.exe", "Code.exe"]
        );
        // without an MRUList the stored order is kept
        assert_eq!(mru_order(&values[..2]), ["notepad.exe", "Code.exe"]);
        // an MRU letter without a value is skipped; duplicates collapse
        let odd = vec![
            pair("a", "x.exe"),
            pair("b", "X.EXE"),
            pair("MRUList", "zab"),
        ];
        assert_eq!(mru_order(&odd), ["x.exe"]);
    }

    #[test]
    fn labels() {
        assert_eq!(plain_label("  WordPad "), Some("WordPad".into()));
        assert_eq!(plain_label("@{Microsoft.Photos?ms-resource://x}"), None);
        assert_eq!(plain_label(""), None);
        assert_eq!(stem_label(r"C:\Windows\system32\notepad.exe"), "Notepad");
        assert_eq!(stem_label("code"), "Code");
    }

    #[test]
    fn duplicates_by_label_or_program_collapse() {
        let h = |name: &str, program: &str| OpenHandler {
            name: name.into(),
            command: CommandSpec {
                program: program.into(),
                args: vec!["{file}".into()],
            },
        };
        let mut list = Vec::new();
        push_unique(&mut list, h("Notepad", r"C:\Windows\notepad.exe"));
        push_unique(&mut list, h("Text editor", r"D:\other\NOTEPAD.EXE"));
        push_unique(&mut list, h("notepad", "x.exe"));
        push_unique(&mut list, h("Code", "code.exe"));
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn arguments_are_env_expanded_too() {
        let spec = command_to_spec(
            r#"rundll32.exe "%SystemRoot%\viewer.dll", ImageView_Fullscreen %1"#,
            env,
        )
        .unwrap();
        assert_eq!(spec.args[0], r"C:\Windows\viewer.dll,");
        assert_eq!(spec.args.last().unwrap(), "{file}");
    }

    type Table = Vec<(&'static str, &'static str)>;

    #[derive(Default)]
    struct Fake {
        user_list: Vec<(String, String)>,
        user_progids: Vec<String>,
        classes_list: Vec<String>,
        classes_progids: Vec<String>,
        commands: Table,
        app_paths: Table,
        friendly: Table,
    }

    fn find(table: &Table, key: &str) -> Option<String> {
        table
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| (*v).to_owned())
    }

    impl RegistrySource for Fake {
        fn user_open_with_list(&self, _: &str) -> Vec<(String, String)> {
            self.user_list.clone()
        }
        fn user_progids(&self, _: &str) -> Vec<String> {
            self.user_progids.clone()
        }
        fn classes_open_with_list(&self, _: &str) -> Vec<String> {
            self.classes_list.clone()
        }
        fn classes_progids(&self, _: &str) -> Vec<String> {
            self.classes_progids.clone()
        }
        fn application_command(&self, exe: &str) -> Option<String> {
            find(&self.commands, &format!("app:{exe}"))
        }
        fn app_path(&self, exe: &str) -> Option<String> {
            find(&self.app_paths, exe)
        }
        fn find_in_path(&self, _: &str) -> Option<String> {
            None
        }
        fn progid_command(&self, progid: &str) -> Option<String> {
            find(&self.commands, progid)
        }
        fn application_friendly_name(&self, exe: &str) -> Option<String> {
            find(&self.friendly, &format!("app:{exe}"))
        }
        fn progid_friendly_name(&self, progid: &str) -> Option<String> {
            find(&self.friendly, progid)
        }
        fn env(&self, name: &str) -> Option<String> {
            env(name)
        }
    }

    fn pair(a: &str, b: &str) -> (String, String) {
        (a.to_owned(), b.to_owned())
    }

    #[test]
    fn collect_merges_user_mru_then_classes_then_progids() {
        let fake = Fake {
            user_list: vec![
                pair("a", "notepad.exe"),
                pair("b", "code.exe"),
                pair("MRUList", "ba"),
            ],
            user_progids: vec!["txtfile".into()],
            classes_list: vec!["wordpad.exe".into(), "NOTEPAD.EXE".into()],
            classes_progids: vec!["MyApp.txt".into(), String::new()],
            commands: vec![
                ("app:notepad.exe", r#""C:\Windows\notepad.exe" "%1""#),
                ("app:code.exe", r#""C:\Code\Code.exe" "%1""#),
                ("app:wordpad.exe", r#"%SystemRoot%\wordpad.exe "%1""#),
                ("txtfile", r"%SystemRoot%\system32\NOTEPAD.EXE %1"),
                ("MyApp.txt", r#""D:\apps\myapp.exe" --open "%1""#),
            ],
            friendly: vec![
                ("app:code.exe", "Visual Studio Code"),
                ("MyApp.txt", "My app"),
            ],
            ..Fake::default()
        };
        let names: Vec<String> = collect(&fake, ".txt").into_iter().map(|h| h.name).collect();
        // code first (MRU), notepad, wordpad; the txtfile ProgId is notepad again and collapses
        assert_eq!(
            names,
            ["Visual Studio Code", "Notepad", "Wordpad", "My app"]
        );
    }

    #[test]
    fn collect_resolves_programs_through_app_paths() {
        let fake = Fake {
            classes_list: vec!["tool.exe".into(), "ghost.exe".into()],
            app_paths: vec![("tool.exe", r"C:\Tools\tool.exe")],
            ..Fake::default()
        };
        let handlers = collect(&fake, ".x");
        assert_eq!(handlers.len(), 1, "unresolvable programs are skipped");
        assert_eq!(handlers[0].command.program, r"C:\Tools\tool.exe");
        assert_eq!(handlers[0].command.args, ["{file}"]);
    }

    #[test]
    fn collect_of_an_empty_registry_is_empty() {
        assert!(collect(&Fake::default(), ".txt").is_empty());
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "reads the real registry of the machine"]
    fn txt_has_registered_handlers() {
        #![allow(clippy::print_stderr)]
        let handlers = registry_handlers(".txt");
        assert!(!handlers.is_empty(), "no handlers found for .txt");
        for h in &handlers {
            assert!(!h.name.is_empty());
            assert!(h.command.args.iter().any(|a| a.contains("{file}")));
        }
        eprintln!("{handlers:#?}");
    }
}
