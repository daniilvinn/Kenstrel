# Security policy

## Supported versions

Kestrel is pre-1.0. Only the latest release receives security fixes.

| Version | Supported |
|---|---|
| latest 0.x release | yes |
| older releases | no |

## Reporting a vulnerability

Please do **not** open a public issue or pull request for a security problem.

Use GitHub's private vulnerability reporting: open the repository's
[Security tab](https://github.com/daniilvinn/Kestrel/security) and choose
**Report a vulnerability**. Include the Kestrel version (`ks --version`), your Windows version
and terminal, what you found, and steps or a sample file or directory that reproduces it.

This is a personal project maintained in spare time, so please allow some days for a first
reply. Valid reports will be fixed in a new release and credited in the release notes unless you
prefer to stay anonymous.

## Scope and design notes

Things Kestrel does on purpose, which are useful to know when judging a report:

- **Claude credentials are never touched.** Kestrel only starts your own installed `claude` CLI.
  It never reads, copies or stores Claude credentials or tokens, and it removes API-billing
  environment variables (`ANTHROPIC_API_KEY` and others) from every `claude` process it starts.
  Its MCP server and hook are injected per spawn, and no `.mcp.json` is written into your
  projects. The local channel between those helpers and Kestrel is a per-process named pipe.
- **Terminal output is sanitised.** File content shown in the preview is passed through a
  sanitiser that replaces control characters, escape sequences, C1 controls and bidirectional
  text controls, so a malicious file cannot inject escape sequences into your terminal.
- **Git is invoked defensively.** All `git` calls go through one hardened builder: no optional
  locks, no pager, no credential prompts, `core.fsmonitor` disabled and the `ext` protocol
  disallowed, so browsing an untrusted repository does not execute programs configured in its
  `.git/config`.
- **No network access of its own.** Kestrel itself makes no network requests. It launches programs
  you ask it to (your editor, the default application for a file, `claude`, shell commands you
  type with `!`), which run with your privileges.
- **`unsafe` code** is forbidden except in the Windows platform module of `ks-fs`.
- Symlinks and junctions are not followed while searching, and OneDrive online-only files are
  never downloaded by preview or search.

Reports about the behaviour of Claude Code itself, Windows Terminal or third-party dependencies
should go to those projects (but do tell me if a dependency issue affects Kestrel).
