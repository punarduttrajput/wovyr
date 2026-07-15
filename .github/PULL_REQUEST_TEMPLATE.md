## What & why

<!-- What does this change do, and what problem prompted it? Link the issue if one exists. -->

## How it was verified

<!-- Tests added/updated, and what you actually ran (`cargo test -p ...`, a live run, ...).
     House rule: tests ship with code; bug fixes get a regression test. -->

## Checklist

- [ ] `cargo fmt --all --check` and `cargo clippy --workspace --all-targets -- -D warnings` pass
- [ ] Tests cover the change (`cargo test --workspace`)
- [ ] Docs updated if behavior changed (the linked `docs/` spec is the source of truth)
- [ ] Commits are signed off (`git commit -s`) — see the DCO section in CONTRIBUTING.md
