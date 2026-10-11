## Summary

<!-- What does this change and why? Link the issue it closes: "Closes #123". -->

## Type

<!-- The PR title must follow Conventional Commits: type(scope): summary. -->

- [ ] feat
- [ ] fix
- [ ] perf
- [ ] refactor
- [ ] docs
- [ ] test
- [ ] build / ci / chore / style
- [ ] Breaking change (title has `!`, explained above)

## Checklist

- [ ] `cargo fmt --all --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace`
- [ ] Tests added or updated for the change
- [ ] Snapshots reviewed and accepted if the UI changed (`cargo insta review`)
- [ ] `docs/keymap.md` regenerated if default keys or commands changed
- [ ] README and `CHANGELOG.md` updated for user-visible changes
- [ ] The Claude integration rules in CONTRIBUTING.md still hold (if touched)

## Screenshots

<!-- For UI changes: before and after, in Windows Terminal if possible. -->
