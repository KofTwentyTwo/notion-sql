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

## Local development gates

This repo enforces formatting, linting, documentation, license headers, and
supply-chain policy. To run the same checks locally:

- One-time setup (installs the git hooks): `just setup`
  (or `git config core.hooksPath .githooks`)
- Fast gate (runs automatically on commit): `just check`
  — `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `./scripts/check-headers.sh`
- Full gate (run before pushing): `just test`, `just deny`, `just doc`

Tooling required: `rustfmt` + `clippy` (rustup components), and for the full gate
`cargo install cargo-deny`. `just` is optional — the hook and CI call `cargo` directly.

Every `.rs` file must begin with:

    // SPDX-License-Identifier: MIT
    // Copyright (c) 2026 James Maes

All public and private items must carry doc comments — `missing_docs` and
`clippy::missing_docs_in_private_items` are denied (they fail the build, not just CI).

## Branches

Use `feature/*` branches for pull requests into `develop`.

Release candidates stabilize on `release/*` or `rc/*` branches. Production releases are tagged from `main`.

## Pull Requests

Pull requests should include:

- A short description of the change.
- Tests for parser, filter, or value-coercion behavior when those paths change.
- Notes about Notion API behavior if the change affects API payloads.

Mutating Notion behavior should stay dry-run by default unless the user explicitly passes `--apply`.
