//! Credential / billing environment stripping (plan 10.1).
//!
//! API keys take precedence over the subscription login, so every spawned `claude` process
//! (preflight, embedded pane, WT pane command) has these variables removed.

/// Default variables removed from every spawned `claude` environment.
pub const DEFAULT_STRIP_ENV: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
];

/// Further variables that also switch billing or route requests away from the subscription
/// login. Not in the default list because the WT command string is specified verbatim by plan
/// 10.2 and some users legitimately set a proxy base URL; opt in with [`EnvPolicy::extended`].
pub const EXTRA_STRIP_ENV: &[&str] = &[
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_FOUNDRY_API_KEY",
    "AWS_BEARER_TOKEN_BEDROCK",
];

/// Which environment variables to strip. Configurable; defaults to [`DEFAULT_STRIP_ENV`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvPolicy {
    /// Variable names to remove.
    pub strip: Vec<String>,
}

impl Default for EnvPolicy {
    fn default() -> Self {
        Self {
            strip: DEFAULT_STRIP_ENV.iter().map(|s| (*s).to_string()).collect(),
        }
    }
}

impl EnvPolicy {
    /// The default list plus [`EXTRA_STRIP_ENV`].
    pub fn extended() -> Self {
        Self::new(DEFAULT_STRIP_ENV.iter().chain(EXTRA_STRIP_ENV).copied())
    }

    /// A policy with a custom list.
    pub fn new<I, S>(names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            strip: names.into_iter().map(Into::into).collect(),
        }
    }

    /// Strip from a `std::process::Command`.
    pub fn apply_std(&self, cmd: &mut std::process::Command) {
        for name in &self.strip {
            cmd.env_remove(name);
        }
    }

    /// Strip from a `portable_pty::CommandBuilder`.
    pub fn apply_pty(&self, cmd: &mut portable_pty::CommandBuilder) {
        for name in &self.strip {
            cmd.env_remove(name);
        }
    }

    /// Names that are valid PowerShell env identifiers (alphanumerics and `_`). Anything else is
    /// dropped when generating shell text, to rule out injection through configuration.
    pub fn shell_safe_names(&self) -> Vec<&str> {
        self.strip
            .iter()
            .map(String::as_str)
            .filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn std_command_marks_vars_removed() {
        let mut cmd = std::process::Command::new("x");
        EnvPolicy::default().apply_std(&mut cmd);
        let removed: Vec<_> = cmd
            .get_envs()
            .filter(|(_, v)| v.is_none())
            .map(|(k, _)| k.to_owned())
            .collect();
        for name in DEFAULT_STRIP_ENV {
            assert!(removed.iter().any(|k| k == OsStr::new(name)), "{name}");
        }
    }

    #[test]
    fn pty_command_removes_vars() {
        let mut cmd = portable_pty::CommandBuilder::new("x");
        cmd.env("ANTHROPIC_API_KEY", "secret");
        EnvPolicy::default().apply_pty(&mut cmd);
        assert!(cmd.get_env("ANTHROPIC_API_KEY").is_none());
    }

    #[test]
    fn shell_safe_filters_bad_names() {
        let p = EnvPolicy::new(["OK_1", "bad name", "x;y", ""]);
        assert_eq!(p.shell_safe_names(), vec!["OK_1"]);
    }

    #[cfg(windows)]
    #[test]
    fn resulting_child_environment_lacks_the_variables() {
        // COMPUTERNAME stands in for a credential variable; names are case-insensitive.
        let probe = |policy: Option<EnvPolicy>| {
            let mut cmd = std::process::Command::new("cmd");
            cmd.args(["/c", "if defined COMPUTERNAME (echo SET) else (echo UNSET)"]);
            if let Some(p) = policy {
                p.apply_std(&mut cmd);
            }
            String::from_utf8_lossy(&cmd.output().unwrap().stdout)
                .trim()
                .to_string()
        };
        assert_eq!(probe(None), "SET");
        assert_eq!(probe(Some(EnvPolicy::new(["computername"]))), "UNSET");
    }

    #[cfg(windows)]
    #[test]
    fn pty_removal_is_case_insensitive() {
        let mut cmd = portable_pty::CommandBuilder::new("x");
        cmd.env("Anthropic_Api_Key", "secret");
        EnvPolicy::default().apply_pty(&mut cmd);
        assert!(cmd.get_env("ANTHROPIC_API_KEY").is_none());
        assert!(cmd.get_env("Anthropic_Api_Key").is_none());
    }

    #[test]
    fn default_list_contains_the_spec_variables_and_extended_adds_more() {
        assert!(!EnvPolicy::default()
            .strip
            .iter()
            .any(|s| s == "ANTHROPIC_BASE_URL"));
        let p = EnvPolicy::extended();
        for n in [
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CODE_USE_FOUNDRY",
            "ANTHROPIC_BASE_URL",
        ] {
            assert!(p.strip.iter().any(|s| s == n), "{n}");
        }
    }
}
