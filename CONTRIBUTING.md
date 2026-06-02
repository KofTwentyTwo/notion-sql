# Contributing

## Development Setup

Install the Rust toolchain and the release tooling listed in `README.md`.

Useful local checks:

```bash
cargo fmt
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo build --release
```

## Branches

Use `feature/*` branches for pull requests into `develop`.

Release candidates stabilize on `release/*` or `rc/*` branches. Production releases are tagged from `main`.

## Pull Requests

Pull requests should include:

- A short description of the change.
- Tests for parser, filter, or value-coercion behavior when those paths change.
- Notes about Notion API behavior if the change affects API payloads.

Mutating Notion behavior should stay dry-run by default unless the user explicitly passes `--apply`.
