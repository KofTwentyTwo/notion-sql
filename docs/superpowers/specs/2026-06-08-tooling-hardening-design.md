# Tooling & Enforcement Hardening — Design Spec

**Date:** 2026-06-08
**Status:** Approved (post clean-room review)
**Crate:** `notion-sql` v1.0.0 — Rust lib + binary, edition 2021, MIT, ~188 transitive deps.

## Goal

Add linting, style, security/supply-chain, and documentation/header enforcement to
`notion-sql`, gated both locally (pre-commit) and in CI. Make the documentation
already present in the tree (`//!` module headers + `///` on every item)
*enforced* so it cannot regress.

## Locked decisions

| Area | Decision |
|------|----------|
| Doc enforcement | Escalate `missing_docs` + `clippy::missing_docs_in_private_items` to **deny** via a `Cargo.toml [lints]` table (package-wide). |
| File headers | SPDX + copyright banner on every `.rs` file, enforced by a script. |
| MSRV | Determined empirically with `cargo-msrv`, pinned in `Cargo.toml`, with a CI job on the discovered floor. |
| Supply chain | **`cargo-deny` only** (advisories + licenses + bans + sources). `cargo-audit` dropped as redundant. |
| Local hooks | POSIX `.githooks/pre-commit` (no `just` dependency) + an optional `justfile` convenience wrapper. |
| Docs gate | Add a rustdoc broken-intra-doc-link gate (`RUSTDOCFLAGS="-D warnings"`). |
| Formatting | Add a minimal `rustfmt.toml` (`edition = "2021"`) for deterministic `cargo fmt --check`. |

## Components

### 1. Doc-comment enforcement (`Cargo.toml`)
Remove the per-file `#![warn(missing_docs)]` / `#![warn(clippy::missing_docs_in_private_items)]`
attributes from `src/lib.rs` and `src/main.rs`. Replace with a package-level table:

```toml
[lints.rust]
missing_docs = "deny"

[lints.clippy]
missing_docs_in_private_items = "deny"
```

Verified (clean-room): the `[lints]` table propagates to `tests/` integration crates,
and the current tree already passes at `deny` (no code changes required).

### 2. SPDX / copyright headers
Prepend to every `src/*.rs` and `tests/*.rs` file, **above** the existing `//!`:

```rust
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 James Maes
```

(Matches the `LICENSE` copyright line exactly. Plain `//` comments above `//!` and
`#![...]` inner attributes compile cleanly — verified.)

Enforcement: `scripts/check-headers.sh` iterates `git ls-files '*.rs'` (NOT `find`,
to avoid `target/`/generated files) and fails if the first lines lack the SPDX id.
Wired into the pre-commit hook and a CI step.

### 3. MSRV
- Run `cargo install cargo-msrv` then `cargo msrv find` to determine the true lowest
  Rust that compiles the crate.
- Set `rust-version = "<discovered>"` in `Cargo.toml [package]`.
- Add a CI job using `dtolnay/rust-toolchain@<discovered>` that builds + tests, so a
  future change that raises the floor fails fast.

### 4. Supply chain (`deny.toml` + CI)
Generate `deny.toml` with `cargo deny init` (ensures the **current v2 schema** — the
old `unlicensed`/`copyleft`/`default` keys are gone). Then configure:
- `[licenses]` allow = `["MIT", "Apache-2.0", "BSD-2-Clause", "BSD-3-Clause", "ISC", "Unicode-3.0", "CDLA-Permissive-2.0"]`
  — `Unicode-3.0` and `CDLA-Permissive-2.0` are **required** (the latter is `webpki-roots`,
  standalone; the former is `unicode-ident` et al.). Other licenses resolve via OR
  expressions.
- `[advisories]` — deny on RustSec advisories; `yanked = "deny"`.
- `[bans]` — `multiple-versions = "warn"`.
- `[sources]` — `unknown-registry = "deny"`, allow crates.io only.

CI: `EmbarkStudios/cargo-deny-action` (current recommended action; bundles install).
Verify locally with `cargo deny check` before pushing.

### 5. Local enforcement (`justfile` + `.githooks/`)
- `justfile` recipes (optional convenience): `fmt`, `lint`, `test`, `headers`, `deny`,
  `doc`, `check` (fmt --check + clippy + headers), `setup`.
- `.githooks/pre-commit`: a **POSIX `sh` script** calling `cargo fmt --check`,
  `cargo clippy --all-targets --all-features -- -D warnings`, and
  `scripts/check-headers.sh` directly — no `just` dependency. Tests are **not** in
  pre-commit (kept fast; full tests run in CI).
- `just setup` runs `git config core.hooksPath .githooks` (one-time per clone — git
  has no fully-automatic committed-hook mechanism). Documented in `CONTRIBUTING.md`.

### 6. CI (`.github/workflows/ci.yml`)
Add a `quality` job (parallel to `rust-checks`, `permissions: contents: read`):
- `cargo deny check` (via the action),
- license-header check (`scripts/check-headers.sh`),
- rustdoc gate: `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`.
Add the MSRV job from §3. Existing fmt/clippy/test/build steps are unchanged (they
already enforce docs via `-D warnings`).

### 7. `rustfmt.toml`
```toml
edition = "2021"
```

## Enforcement matrix

| Check | Tool | Scope | Where |
|-------|------|-------|-------|
| Public-item docs | rustc `missing_docs` (deny) | `src/` + `tests/` (pub only) | local build + CI |
| Private-item docs | clippy `missing_docs_in_private_items` (deny) | `src/` only | clippy in hook + CI |
| SPDX headers | `check-headers.sh` | all tracked `.rs` | hook + CI |
| Advisories/licenses/bans/sources | cargo-deny | dep tree | CI |
| Broken doc links | rustdoc `-D warnings` | crate | CI |
| Formatting | `cargo fmt --check` | all | hook + CI |
| MSRV | pinned toolchain build | crate | CI |

## Accepted caveats
- **`tests/` private-item docs are not lint-enforceable** — clippy's private-docs lint
  does not fire on integration test crates (verified). Only rustc `missing_docs`
  (public items) enforces anything in `tests/`. Accepted.
- **Git hooks require one-time `just setup` / `git config` per clone.** CI is the real
  gate; hooks are best-effort local UX.
- **MSRV value is computed by `cargo-msrv` at implementation time**, then pinned.

## Out of scope (YAGNI)
- `cargo-audit` (redundant with cargo-deny advisories).
- `dependabot` (already present: `.github/dependabot.yml`).
- `CHANGELOG.md` (already present; drives cargo-dist release notes).
- `cargo xtask`, the pre-commit.com framework, commit-message linting, `clippy.toml`.

## Verification (acceptance)
All must pass after implementation:
1. `cargo fmt --check`
2. `cargo clippy --all-targets --all-features -- -D warnings`
3. `cargo test --all-targets --all-features`
4. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
5. `cargo deny check`
6. `scripts/check-headers.sh` (exit 0; all `.rs` files carry the SPDX banner)
7. Build on the pinned MSRV toolchain
8. `.githooks/pre-commit` runs green after `just setup`
