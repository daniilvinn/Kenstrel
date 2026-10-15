//! Make Kestrel the default file manager (`ks --set-default`) and give the job back to File
//! Explorer (`ks --unset-default`). Windows only, per user, no administrator rights needed.
//!
//! Everything lives under `HKCU\Software\Classes`, which overrides the machine-wide classes:
//!
//! - `Directory\shell` and `Drive\shell` get a `kestrel` verb and make it the default verb, so
//!   opening a folder or drive anywhere in the shell runs [`Target::folder_command`]. Explorer's
//!   own `open` verb stays in the context menu.
//! - `CLSID\{52205fd8-...}\shell\opennewwindow\command` (the "This PC" folder) runs
//!   [`Target::home_command`], which is what Win+E opens. `DelegateExecute` is set to an empty
//!   string so the shell runs the command instead of Explorer's COM handler.
//!
//! What the commands start is up to the caller (`ks-app` builds them); this module only
//! writes and undoes registry values.
//!
//! Before writing, the previous value of every touched registry value, the value written and
//! every key that did not exist yet are recorded under [`BACKUP_KEY`]. Unsetting restores the
//! previous values (only where they still hold what Kestrel wrote, so a later change by another
//! program survives), removes the keys Kestrel created once they are empty, and deletes the
//! backup.
//!
//! The planning is portable and tested against an in-memory [`Registry`]; [`set_default`] and
//! [`unset_default`] run it against the real registry.

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;

/// Name of the shell verb Kestrel registers.
pub const VERB: &str = "kestrel";
/// Where the backup of the replaced values lives (under `HKCU`).
pub const BACKUP_KEY: &str = r"Software\Kestrel\DefaultFileManager";
/// Keys are created below this one; it always exists and is never removed.
const CLASSES: &str = r"Software\Classes";
/// The "This PC" folder's new-window command, which Win+E runs.
const THIS_PC_COMMAND: &str =
    r"Software\Classes\CLSID\{52205fd8-5dfb-447d-801a-d0b52f2e83e1}\shell\opennewwindow\command";
const BACKUP_CREATED: &str = "CreatedKeys";
/// Prefix of the backup values holding a replaced value (`=data`, or `-` when there was none).
const BACKUP_VALUE_PREFIX: &str = "Value:";
/// Prefix of the backup values holding what Kestrel wrote there.
const BACKUP_WROTE_PREFIX: &str = "Wrote:";

/// One registry string value Kestrel writes (`name` empty for the default value).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    /// Key path under `HKCU`.
    pub key: String,
    /// Value name; empty for the key's default value.
    pub name: String,
    /// Data written.
    pub value: String,
}

/// What a call changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The registry was changed.
    Changed,
    /// Nothing to do: Kestrel already was (or was not) the default.
    Unchanged,
}

/// Minimal string-valued view of `HKCU`. Key paths and value names are case-insensitive.
pub trait Registry {
    /// A string value, `None` when the value (or its key) does not exist.
    fn get(&self, key: &str, name: &str) -> io::Result<Option<String>>;
    /// Names of the key's values; empty when the key does not exist.
    fn value_names(&self, key: &str) -> io::Result<Vec<String>>;
    /// Create the key (and its parents) as needed and write a string value.
    fn set(&mut self, key: &str, name: &str, value: &str) -> io::Result<()>;
    /// Delete a value; succeeds when it does not exist.
    fn delete_value(&mut self, key: &str, name: &str) -> io::Result<()>;
    /// Whether the key exists.
    fn key_exists(&self, key: &str) -> io::Result<bool>;
    /// Delete the key if it exists and has neither values nor sub keys.
    fn remove_key_if_empty(&mut self, key: &str) -> io::Result<()>;
    /// Delete the key with all its values and sub keys; succeeds when it does not exist.
    fn delete_tree(&mut self, key: &str) -> io::Result<()>;
}

/// What the registered verbs run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// Icon of the context-menu entry (`ks.exe`).
    pub icon: PathBuf,
    /// Opens a folder or drive; `%1` stands for its path.
    pub folder_command: String,
    /// Opens the home directory (Win+E).
    pub home_command: String,
}

impl Target {
    /// The values that make this target the default file manager.
    pub fn edits(&self) -> Vec<Edit> {
        let mut out = Vec::new();
        let mut add = |key: String, name: &str, value: String| {
            out.push(Edit {
                key,
                name: name.to_owned(),
                value,
            });
        };
        for class in ["Directory", "Drive"] {
            let shell = format!(r"{CLASSES}\{class}\shell");
            add(shell.clone(), "", VERB.to_owned());
            add(format!(r"{shell}\{VERB}"), "", "Open in Kestrel".to_owned());
            add(
                format!(r"{shell}\{VERB}"),
                "Icon",
                format!("\"{}\"", self.icon.display()),
            );
            add(
                format!(r"{shell}\{VERB}\command"),
                "",
                self.folder_command.clone(),
            );
        }
        add(THIS_PC_COMMAND.to_owned(), "", self.home_command.clone());
        add(THIS_PC_COMMAND.to_owned(), "DelegateExecute", String::new());
        out
    }
}

/// Whether Kestrel is the default folder handler right now.
pub fn is_default(reg: &dyn Registry) -> io::Result<bool> {
    let current = reg.get(&format!(r"{CLASSES}\Directory\shell"), "")?;
    Ok(current.is_some_and(|v| v.eq_ignore_ascii_case(VERB)))
}

fn slot(key: &str, name: &str) -> String {
    format!("{key}|{name}")
}

/// A replaced value as stored in the backup: `=data` when it existed, `-` when it did not.
fn encode_previous(previous: Option<&str>) -> String {
    previous.map_or_else(|| "-".to_owned(), |v| format!("={v}"))
}

fn decode_previous(stored: &str) -> Option<&str> {
    stored.strip_prefix('=')
}

/// `key` and its ancestors below [`CLASSES`], deepest first.
fn ancestors_below_classes(key: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = key;
    while current.len() > CLASSES.len() && current[..CLASSES.len()].eq_ignore_ascii_case(CLASSES) {
        out.push(current.to_owned());
        match current.rfind('\\') {
            Some(i) => current = &current[..i],
            None => break,
        }
    }
    out
}

/// Apply the target's edits, recording what they replace. Running it again keeps the first
/// backup, so the state from before the first `--set-default` is what unsetting returns to.
pub fn apply(reg: &mut dyn Registry, target: &Target) -> io::Result<Outcome> {
    let edits = target.edits();
    let mut pending = Vec::new();
    for edit in &edits {
        let current = reg.get(&edit.key, &edit.name)?;
        if current.as_deref() != Some(edit.value.as_str()) {
            pending.push((edit, current));
        }
    }
    if pending.is_empty() && reg.key_exists(BACKUP_KEY)? {
        return Ok(Outcome::Unchanged);
    }

    // Keys that do not exist yet, recorded before anything is created.
    let mut created: Vec<String> = reg
        .get(BACKUP_KEY, BACKUP_CREATED)?
        .map(|v| v.lines().map(str::to_owned).collect())
        .unwrap_or_default();
    for edit in &edits {
        for key in ancestors_below_classes(&edit.key) {
            if !created.iter().any(|c| c.eq_ignore_ascii_case(&key)) && !reg.key_exists(&key)? {
                created.push(key);
            }
        }
    }

    // The backup is complete before the first real value is touched.
    for (edit, current) in &pending {
        let name = format!("{BACKUP_VALUE_PREFIX}{}", slot(&edit.key, &edit.name));
        if reg.get(BACKUP_KEY, &name)?.is_none() {
            reg.set(BACKUP_KEY, &name, &encode_previous(current.as_deref()))?;
        }
    }
    for edit in &edits {
        let name = format!("{BACKUP_WROTE_PREFIX}{}", slot(&edit.key, &edit.name));
        reg.set(BACKUP_KEY, &name, &edit.value)?;
    }
    reg.set(BACKUP_KEY, BACKUP_CREATED, &created.join("\n"))?;

    for (edit, _) in &pending {
        reg.set(&edit.key, &edit.name, &edit.value)?;
    }
    Ok(Outcome::Changed)
}

/// Undo [`apply`]. Values the backup does not describe (registered by hand, or by a build
/// that kept no record) count as Kestrel's when they equal what `fallback` would write.
pub fn revert(reg: &mut dyn Registry, fallback: &Target) -> io::Result<Outcome> {
    let has_backup = reg.key_exists(BACKUP_KEY)?;
    // slot -> what Kestrel wrote there.
    let mut wrote: BTreeMap<String, (String, String, String)> = BTreeMap::new();
    for edit in fallback.edits() {
        let slot = slot(&edit.key, &edit.name).to_ascii_lowercase();
        wrote.insert(slot, (edit.key, edit.name, edit.value));
    }
    for name in reg.value_names(BACKUP_KEY)? {
        let prefix = BACKUP_WROTE_PREFIX.len();
        let Some(slot) = name
            .get(..prefix)
            .filter(|p| p.eq_ignore_ascii_case(BACKUP_WROTE_PREFIX))
            .map(|_| &name[prefix..])
        else {
            continue;
        };
        let Some((key, value_name)) = slot.split_once('|') else {
            continue;
        };
        if let Some(value) = reg.get(BACKUP_KEY, &name)? {
            wrote.insert(
                slot.to_ascii_lowercase(),
                (key.to_owned(), value_name.to_owned(), value),
            );
        }
    }

    let mut changed = false;
    for (key, name, value) in wrote.values() {
        // Only Kestrel's own values are put back; anything changed since is left alone.
        if reg.get(key, name)?.as_deref() != Some(value.as_str()) {
            continue;
        }
        let previous = reg.get(
            BACKUP_KEY,
            &format!("{BACKUP_VALUE_PREFIX}{}", slot(key, name)),
        )?;
        match previous.as_deref().and_then(decode_previous) {
            Some(old) => reg.set(key, name, old)?,
            None => reg.delete_value(key, name)?,
        }
        changed = true;
    }

    let mut created: Vec<String> = match reg.get(BACKUP_KEY, BACKUP_CREATED)? {
        Some(list) => list.lines().map(str::to_owned).collect(),
        // Without a record, only the verb keys are known to be Kestrel's.
        None => fallback
            .edits()
            .into_iter()
            .filter(|e| e.key.contains(&format!(r"\{VERB}")))
            .map(|e| e.key)
            .collect(),
    };
    // Children before parents.
    created.sort_by_key(|k| std::cmp::Reverse(k.len()));
    created.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    for key in &created {
        if reg.key_exists(key)? {
            reg.remove_key_if_empty(key)?;
            changed |= !reg.key_exists(key)?;
        }
    }

    if has_backup {
        reg.delete_tree(BACKUP_KEY)?;
        changed = true;
    }
    reg.remove_key_if_empty(r"Software\Kestrel")?;
    Ok(if changed {
        Outcome::Changed
    } else {
        Outcome::Unchanged
    })
}

/// Register `target` as the default file manager for the current user.
pub fn set_default(target: &Target) -> io::Result<Outcome> {
    #[cfg(windows)]
    {
        let mut reg = win::CurrentUser;
        let outcome = apply(&mut reg, target)?;
        crate::platform::notify_association_changed();
        Ok(outcome)
    }
    #[cfg(not(windows))]
    {
        let _ = target;
        Err(unsupported())
    }
}

/// Give folders, drives and Win+E back to File Explorer. `fallback` describes the
/// registration to remove where the backup has no record.
pub fn unset_default(fallback: &Target) -> io::Result<Outcome> {
    #[cfg(windows)]
    {
        let mut reg = win::CurrentUser;
        let outcome = revert(&mut reg, fallback)?;
        crate::platform::notify_association_changed();
        Ok(outcome)
    }
    #[cfg(not(windows))]
    {
        let _ = fallback;
        Err(unsupported())
    }
}

#[cfg(not(windows))]
fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "setting the default file manager is only supported on Windows",
    )
}

#[cfg(windows)]
mod win {
    use std::io;

    use winreg::enums::{HKEY_CURRENT_USER, KEY_ALL_ACCESS, KEY_READ};
    use winreg::RegKey;

    use super::Registry;

    /// `HKEY_CURRENT_USER`.
    pub(super) struct CurrentUser;

    fn hkcu() -> RegKey {
        RegKey::predef(HKEY_CURRENT_USER)
    }

    fn not_found(err: &io::Error) -> bool {
        err.kind() == io::ErrorKind::NotFound
    }

    impl Registry for CurrentUser {
        fn get(&self, key: &str, name: &str) -> io::Result<Option<String>> {
            let key = match hkcu().open_subkey_with_flags(key, KEY_READ) {
                Ok(key) => key,
                Err(e) if not_found(&e) => return Ok(None),
                Err(e) => return Err(e),
            };
            match key.get_value::<String, _>(name) {
                Ok(value) => Ok(Some(value)),
                Err(e) if not_found(&e) => Ok(None),
                Err(e) => Err(e),
            }
        }

        fn value_names(&self, key: &str) -> io::Result<Vec<String>> {
            match hkcu().open_subkey_with_flags(key, KEY_READ) {
                Ok(key) => Ok(key.enum_values().flatten().map(|(name, _)| name).collect()),
                Err(e) if not_found(&e) => Ok(Vec::new()),
                Err(e) => Err(e),
            }
        }

        fn set(&mut self, key: &str, name: &str, value: &str) -> io::Result<()> {
            let (key, _) = hkcu().create_subkey(key)?;
            key.set_value(name, &value)
        }

        fn delete_value(&mut self, key: &str, name: &str) -> io::Result<()> {
            let key = match hkcu().open_subkey_with_flags(key, KEY_ALL_ACCESS) {
                Ok(key) => key,
                Err(e) if not_found(&e) => return Ok(()),
                Err(e) => return Err(e),
            };
            match key.delete_value(name) {
                Err(e) if !not_found(&e) => Err(e),
                _ => Ok(()),
            }
        }

        fn key_exists(&self, key: &str) -> io::Result<bool> {
            match hkcu().open_subkey_with_flags(key, KEY_READ) {
                Ok(_) => Ok(true),
                Err(e) if not_found(&e) => Ok(false),
                Err(e) => Err(e),
            }
        }

        fn remove_key_if_empty(&mut self, key: &str) -> io::Result<()> {
            let info = match hkcu().open_subkey_with_flags(key, KEY_READ) {
                Ok(handle) => handle.query_info()?,
                Err(e) if not_found(&e) => return Ok(()),
                Err(e) => return Err(e),
            };
            if info.sub_keys == 0 && info.values == 0 {
                hkcu().delete_subkey(key)?;
            }
            Ok(())
        }

        fn delete_tree(&mut self, key: &str) -> io::Result<()> {
            match hkcu().delete_subkey_all(key) {
                Err(e) if !not_found(&e) => Err(e),
                _ => Ok(()),
            }
        }
    }
}

/// In-memory [`Registry`] for tests.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MemoryRegistry {
    /// Lowercased key path -> (lowercased value name -> data).
    keys: BTreeMap<String, BTreeMap<String, String>>,
}

impl MemoryRegistry {
    fn norm(text: &str) -> String {
        text.to_ascii_lowercase()
    }

    /// Whether any key starts with `prefix` (case-insensitive).
    pub fn has_keys_under(&self, prefix: &str) -> bool {
        let prefix = Self::norm(prefix);
        self.keys.keys().any(|k| k.starts_with(&prefix))
    }

    /// Create `key` and its parents without values.
    pub fn create_key(&mut self, key: &str) {
        let mut path = String::new();
        for part in key.split('\\') {
            if !path.is_empty() {
                path.push('\\');
            }
            path.push_str(&Self::norm(part));
            self.keys.entry(path.clone()).or_default();
        }
    }
}

impl Registry for MemoryRegistry {
    fn get(&self, key: &str, name: &str) -> io::Result<Option<String>> {
        Ok(self
            .keys
            .get(&Self::norm(key))
            .and_then(|values| values.get(&Self::norm(name)))
            .cloned())
    }

    fn value_names(&self, key: &str) -> io::Result<Vec<String>> {
        // Names come back lowercased; the real registry is case-insensitive too.
        Ok(self
            .keys
            .get(&Self::norm(key))
            .map(|values| values.keys().cloned().collect())
            .unwrap_or_default())
    }

    fn set(&mut self, key: &str, name: &str, value: &str) -> io::Result<()> {
        self.create_key(key);
        self.keys
            .entry(Self::norm(key))
            .or_default()
            .insert(Self::norm(name), value.to_owned());
        Ok(())
    }

    fn delete_value(&mut self, key: &str, name: &str) -> io::Result<()> {
        if let Some(values) = self.keys.get_mut(&Self::norm(key)) {
            values.remove(&Self::norm(name));
        }
        Ok(())
    }

    fn key_exists(&self, key: &str) -> io::Result<bool> {
        Ok(self.keys.contains_key(&Self::norm(key)))
    }

    fn remove_key_if_empty(&mut self, key: &str) -> io::Result<()> {
        let key = Self::norm(key);
        let child = format!("{key}\\");
        let empty = self.keys.get(&key).is_some_and(BTreeMap::is_empty)
            && !self.keys.keys().any(|k| k.starts_with(&child));
        if empty {
            self.keys.remove(&key);
        }
        Ok(())
    }

    fn delete_tree(&mut self, key: &str) -> io::Result<()> {
        let key = Self::norm(key);
        let child = format!("{key}\\");
        self.keys.retain(|k, _| *k != key && !k.starts_with(&child));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target_at(exe: &str) -> Target {
        Target {
            icon: PathBuf::from(exe),
            folder_command: format!(r#""wt.exe" -d "%1\." "{exe}""#),
            home_command: format!(r#""wt.exe" -d "C:\Users\me" "{exe}""#),
        }
    }

    fn target() -> Target {
        target_at(r"C:\Tools\ks.exe")
    }

    /// A typical user hive: the classes root exists, Explorer's own keys are machine-wide.
    fn base() -> MemoryRegistry {
        let mut reg = MemoryRegistry::default();
        reg.create_key(CLASSES);
        reg
    }

    fn set(reg: &mut MemoryRegistry) -> Outcome {
        apply(reg, &target()).unwrap()
    }

    fn unset(reg: &mut MemoryRegistry) -> Outcome {
        revert(reg, &target()).unwrap()
    }

    #[test]
    fn edits_register_folders_drives_and_win_e() {
        let edits = target().edits();
        let get = |key: &str, name: &str| {
            edits
                .iter()
                .find(|e| e.key == key && e.name == name)
                .map(|e| e.value.clone())
        };
        for class in ["Directory", "Drive"] {
            let shell = format!(r"Software\Classes\{class}\shell");
            assert_eq!(get(&shell, "").as_deref(), Some("kestrel"));
            assert_eq!(
                get(&format!(r"{shell}\kestrel\command"), ""),
                Some(target().folder_command)
            );
        }
        assert_eq!(get(THIS_PC_COMMAND, ""), Some(target().home_command));
        assert_eq!(get(THIS_PC_COMMAND, "DelegateExecute").as_deref(), Some(""));
    }

    #[test]
    fn set_then_unset_restores_a_clean_hive_exactly() {
        let mut reg = base();
        let before = reg.clone();
        assert_eq!(set(&mut reg), Outcome::Changed);
        assert!(is_default(&reg).unwrap());
        assert!(reg.key_exists(BACKUP_KEY).unwrap());

        assert_eq!(unset(&mut reg), Outcome::Changed);
        assert!(!is_default(&reg).unwrap());
        assert_eq!(reg, before);
    }

    #[test]
    fn other_verbs_and_a_previous_default_verb_survive() {
        let mut reg = base();
        let shell = r"Software\Classes\Directory\shell";
        reg.set(shell, "", "openinfiles").unwrap();
        reg.set(&format!(r"{shell}\VSCode\command"), "", "code.exe \"%V\"")
            .unwrap();
        let before = reg.clone();

        set(&mut reg);
        assert_eq!(reg.get(shell, "").unwrap().as_deref(), Some("kestrel"));
        unset(&mut reg);
        assert_eq!(reg, before);
    }

    #[test]
    fn setting_twice_keeps_the_original_backup() {
        let mut reg = base();
        reg.set(r"Software\Classes\Drive\shell", "", "none")
            .unwrap();
        let before = reg.clone();
        assert_eq!(set(&mut reg), Outcome::Changed);
        assert_eq!(set(&mut reg), Outcome::Unchanged);
        unset(&mut reg);
        assert_eq!(reg, before);
    }

    #[test]
    fn changed_commands_are_rewritten_and_unset_from_another_copy_still_cleans_up() {
        let mut reg = base();
        let before = reg.clone();
        set(&mut reg);
        let moved = target_at(r"D:\bin\ks.exe");
        assert_eq!(apply(&mut reg, &moved).unwrap(), Outcome::Changed);
        let command = r"Software\Classes\Directory\shell\kestrel\command";
        assert_eq!(
            reg.get(command, "").unwrap(),
            Some(moved.folder_command.clone())
        );
        // The backup records what was written, so any copy of ks can undo it.
        revert(&mut reg, &target_at(r"E:\other\ks.exe")).unwrap();
        assert_eq!(reg, before);
    }

    #[test]
    fn values_changed_by_another_program_are_left_alone() {
        let mut reg = base();
        set(&mut reg);
        let shell = r"Software\Classes\Directory\shell";
        reg.set(shell, "", "opus").unwrap();
        unset(&mut reg);
        assert_eq!(reg.get(shell, "").unwrap().as_deref(), Some("opus"));
        assert!(!reg.has_keys_under(&format!(r"{shell}\kestrel")));
        assert!(!reg.key_exists(BACKUP_KEY).unwrap());
    }

    #[test]
    fn unset_without_registration_does_nothing() {
        let mut reg = base();
        let before = reg.clone();
        assert_eq!(unset(&mut reg), Outcome::Unchanged);
        assert_eq!(reg, before);
    }

    #[test]
    fn unset_without_a_backup_removes_the_values_kestrel_would_write() {
        let mut reg = base();
        set(&mut reg);
        reg.delete_tree(BACKUP_KEY).unwrap();
        assert_eq!(unset(&mut reg), Outcome::Changed);
        assert!(!is_default(&reg).unwrap());
        assert!(!reg.has_keys_under(r"Software\Classes\Directory\shell\kestrel"));
        assert_eq!(reg.get(THIS_PC_COMMAND, "").unwrap(), None);
        // Without the record of created keys, empty parent keys stay; they are harmless.
        assert!(!reg.has_keys_under(r"Software\Kestrel"));
    }

    #[test]
    fn a_backup_without_written_values_falls_back_to_the_current_commands() {
        // Earlier builds recorded only the replaced values.
        let mut reg = base();
        let before = reg.clone();
        set(&mut reg);
        for name in reg.value_names(BACKUP_KEY).unwrap() {
            if name.starts_with("wrote:") {
                reg.delete_value(BACKUP_KEY, &name).unwrap();
            }
        }
        unset(&mut reg);
        assert_eq!(reg, before);
    }

    #[test]
    fn ancestors_stop_at_the_classes_root() {
        assert_eq!(
            ancestors_below_classes(r"Software\Classes\Drive\shell\kestrel"),
            [
                r"Software\Classes\Drive\shell\kestrel",
                r"Software\Classes\Drive\shell",
                r"Software\Classes\Drive",
            ]
        );
        assert!(ancestors_below_classes(r"Software\Kestrel").is_empty());
    }
}
