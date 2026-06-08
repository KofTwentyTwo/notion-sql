# Tooling & Enforcement Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add enforced linting, formatting, supply-chain, documentation, and license-header gates to `notion-sql`, both locally (pre-commit) and in CI.

**Architecture:** Config-and-script changes layered onto an existing Rust lib+binary crate. Local artifacts (lint config, header script, deny.toml, hooks) are created first; CI is wired to them last. Each task ends with a verification command and a commit.

**Tech Stack:** Rust 1.96 / cargo, rustfmt, clippy, cargo-deny, cargo-msrv, GitHub Actions, POSIX sh, just (optional).

**Spec:** `docs/superpowers/specs/2026-06-08-tooling-hardening-design.md`

**Working tree note:** The repo currently has the documentation pass (`//!` + `///` comments across all 17 `.rs` files) **uncommitted**. Task 1 commits it to establish a clean baseline before enforcement is layered on. Branch is `develop`. There are also unrelated uncommitted `.idea/*` edits — do NOT stage those; every commit below stages explicit paths only.

---

### Task 1: Commit the documentation baseline

**Files:**
- Modify (commit only): `src/*.rs`, `tests/*.rs`

- [ ] **Step 1: Confirm the documented tree still builds and lints clean**

Run: `cargo clippy --all-targets --all-features -- -D warnings && cargo test --all-targets --all-features`
Expected: clippy `Finished`, all tests pass (32 tests across 9 binaries), exit 0.

- [ ] **Step 2: Stage only the source/test doc changes (not `.idea/`)**

```bash
git add src/ tests/
git status --short
```
Expected: only `src/*.rs` and `tests/*.rs` show as modified (`M`); `.idea/*` remain unstaged.

- [ ] **Step 3: Commit**

```bash
git commit -m "docs: Add comprehensive doc comments across all source and test files

Module-level //! headers, /// doc comments on all items (public and
private), and inline comments on non-obvious logic. Comments only — no
code changes.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```
Expected: commit succeeds.

---

### Task 2: License-header check script (test-first)

**Files:**
- Create: `scripts/check-headers.sh`

- [ ] **Step 1: Write the check script (the "failing test")**

Create `scripts/check-headers.sh`:
```sh
#!/usr/bin/env sh
# Verify every tracked Rust source file begins with the SPDX + copyright banner.
# Driven off `git ls-files` so target/ and any generated .rs are never scanned.
set -eu

EXPECTED_SPDX="// SPDX-License-Identifier: MIT"
EXPECTED_COPYRIGHT="// Copyright (c) 2026 James Maes"
fail=0
count=0

for f in $(git ls-files '*.rs'); do
    count=$((count + 1))
    first=$(head -n 1 "$f")
    second=$(head -n 2 "$f" | tail -n 1)
    if [ "$first" != "$EXPECTED_SPDX" ] || [ "$second" != "$EXPECTED_COPYRIGHT" ]; then
        echo "MISSING/incorrect license header: $f"
        fail=1
    fi
done

if [ "$fail" -ne 0 ]; then
    echo "License header check FAILED. Every .rs file must start with:"
    echo "  $EXPECTED_SPDX"
    echo "  $EXPECTED_COPYRIGHT"
    exit 1
fi
echo "License header check passed ($count files)."
```

- [ ] **Step 2: Make it executable and run it to verify it FAILS (no headers yet)**

Run: `chmod +x scripts/check-headers.sh && ./scripts/check-headers.sh`
Expected: FAIL — lists every `src/*.rs` and `tests/*.rs` as "MISSING/incorrect license header", exit 1.

- [ ] **Step 3: Commit the script**

```bash
git add scripts/check-headers.sh
git commit -m "build: Add SPDX license-header check script"
```

---

### Task 3: Add SPDX headers to all `.rs` files (makes Task 2 pass)

**Files:**
- Modify: all 17 files in `src/*.rs` and `tests/*.rs`

- [ ] **Step 1: Prepend the banner to every tracked `.rs` file**

The banner is exactly these two lines, inserted as the new first two lines of each file (the existing `//!` becomes line 3+; a blank line between banner and `//!` is optional but keep files consistent — insert banner then existing content directly):
```rust
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 James Maes
```

Apply with this loop (idempotent guard: skip files that already have the SPDX line):
```bash
for f in $(git ls-files '*.rs'); do
    if [ "$(head -n 1 "$f")" != "// SPDX-License-Identifier: MIT" ]; then
        printf '// SPDX-License-Identifier: MIT\n// Copyright (c) 2026 James Maes\n%s' "$(cat "$f")" > "$f.tmp"
        mv "$f.tmp" "$f"
    fi
done
```

- [ ] **Step 2: Verify headers present and the crate still compiles**

Run: `./scripts/check-headers.sh`
Expected: PASS — "License header check passed (17 files)." exit 0.

Run: `cargo build --all-targets`
Expected: `Finished`, exit 0. (Plain `//` comments above `//!`/`#![...]` are legal.)

- [ ] **Step 3: Verify formatting is undisturbed**

Run: `cargo fmt --check`
Expected: exit 0 (no diff).

- [ ] **Step 4: Commit**

```bash
git add src/ tests/
git commit -m "build: Add SPDX/copyright headers to all source and test files"
```

---

### Task 4: Escalate doc lints to deny via `[lints]` table

**Files:**
- Modify: `Cargo.toml` (add `[lints]` table)
- Modify: `src/lib.rs` (remove per-file `#![warn(...)]`), `src/main.rs` (remove per-file `#![warn(...)]`)

- [ ] **Step 1: Add the `[lints]` table to `Cargo.toml`**

Append after the `[package]` table (before `[dependencies]` is fine; placement is free):
```toml
[lints.rust]
missing_docs = "deny"

[lints.clippy]
missing_docs_in_private_items = "deny"
```

- [ ] **Step 2: Remove the now-redundant per-file attributes**

In `src/lib.rs` delete the lines:
```rust
#![warn(missing_docs)]
#![warn(clippy::missing_docs_in_private_items)]
```
In `src/main.rs` delete the same two lines. (Leave the `//!` headers and SPDX banner intact.)

- [ ] **Step 3: Verify the crate compiles AND lints at deny across all targets**

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: `Finished`, exit 0. (Verified in clean-room review that the current tree passes at deny.)

Run: `cargo build --all-targets`
Expected: `Finished`, exit 0 — proves `missing_docs` deny does not break a plain build.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml src/lib.rs src/main.rs
git commit -m "build: Enforce doc comments via Cargo.toml [lints] (deny missing_docs)"
```

---

### Task 5: Add `rustfmt.toml`

**Files:**
- Create: `rustfmt.toml`

- [ ] **Step 1: Create `rustfmt.toml`**

```toml
edition = "2021"
```

- [ ] **Step 2: Verify formatting still clean against the pinned edition**

Run: `cargo fmt --check`
Expected: exit 0 (no diff).

- [ ] **Step 3: Commit**

```bash
git add rustfmt.toml
git commit -m "build: Add rustfmt.toml pinning edition for deterministic formatting"
```

---

### Task 6: Determine and pin the MSRV

**Files:**
- Modify: `Cargo.toml` (add `rust-version`)

- [ ] **Step 1: Install cargo-msrv**

Run: `cargo install cargo-msrv --locked`
Expected: installs the `cargo-msrv` binary (may take several minutes).

- [ ] **Step 2: Find the true minimum supported Rust version**

Run: `cargo msrv find --output-format minimal`
Expected: prints a single version, e.g. `1.74.0` (the floor is at least 1.74, since the `[lints]` table requires 1.74+). Record the printed value as `<MSRV>` for the next step.

- [ ] **Step 3: Add `rust-version` to `Cargo.toml`**

In the `[package]` table, add (using the value from Step 2, MAJOR.MINOR form):
```toml
rust-version = "<MSRV>"
```
For example, if Step 2 printed `1.74.0`, write `rust-version = "1.74"`.

- [ ] **Step 4: Verify the manifest is valid**

Run: `cargo metadata --no-deps --format-version 1 >/dev/null && echo OK`
Expected: `OK`, exit 0.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml
git commit -m "build: Pin MSRV (rust-version) determined by cargo-msrv"
```

---

### Task 7: cargo-deny supply-chain gate

**Files:**
- Create: `deny.toml`

- [ ] **Step 1: Install cargo-deny**

Run: `cargo install cargo-deny --locked`
Expected: installs the `cargo-deny` binary.

- [ ] **Step 2: Generate a schema-correct baseline, then overwrite with our config**

Run: `cargo deny init` (creates a v2-schema `deny.toml`), then replace its contents with:
```toml
[advisories]
yanked = "deny"
ignore = []

[licenses]
allow = [
    "MIT",
    "Apache-2.0",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "ISC",
    "Unicode-3.0",
    "CDLA-Permissive-2.0",
]
confidence-threshold = 0.8

[bans]
multiple-versions = "warn"

[sources]
unknown-registry = "deny"
unknown-git = "deny"
allow-registry = ["https://github.com/rust-lang/crates.io-index"]
```

- [ ] **Step 3: Run the gate**

Run: `cargo deny check`
Expected: `advisories ok`, `bans ok`, `licenses ok`, `sources ok` — exit 0. If a license error appears for a crate whose SPDX id is not in `allow`, add that exact id to the `allow` list (or a per-crate `[[licenses.exceptions]]`) and re-run until clean. Do NOT broaden to copyleft licenses.

- [ ] **Step 4: Commit**

```bash
git add deny.toml
git commit -m "build: Add cargo-deny config (advisories, license allowlist, bans, sources)"
```

---

### Task 8: Local hooks — `justfile` + POSIX pre-commit

**Files:**
- Create: `justfile`
- Create: `.githooks/pre-commit`

- [ ] **Step 1: Create `justfile` (optional convenience wrapper)**

```just
# Developer convenience wrapper. Every recipe maps to a raw cargo command.
set shell := ["bash", "-cu"]

# Fast local gate — mirrors .githooks/pre-commit.
check: fmt-check lint headers

# Formatting (check only / apply).
fmt-check:
    cargo fmt --check
fmt:
    cargo fmt

# Clippy with warnings denied across all targets.
lint:
    cargo clippy --all-targets --all-features -- -D warnings

# Full test suite.
test:
    cargo test --all-targets --all-features

# SPDX license-header check.
headers:
    ./scripts/check-headers.sh

# Supply-chain / license gate.
deny:
    cargo deny check

# Docs with broken-link detection.
doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --no-deps

# One-time per clone: point git at the committed hooks.
setup:
    git config core.hooksPath .githooks
    @echo "Hooks installed (core.hooksPath = .githooks)."
```

- [ ] **Step 2: Create `.githooks/pre-commit` (no `just` dependency)**

```sh
#!/usr/bin/env sh
# Fast local gate. Cheap checks only; full tests + cargo-deny run in CI.
set -eu

echo "pre-commit: cargo fmt --check"
cargo fmt --check

echo "pre-commit: cargo clippy"
cargo clippy --all-targets --all-features -- -D warnings

echo "pre-commit: license headers"
./scripts/check-headers.sh

echo "pre-commit: OK"
```

- [ ] **Step 3: Make the hook executable and wire it up**

Run: `chmod +x .githooks/pre-commit && git config core.hooksPath .githooks`
Expected: exit 0.

- [ ] **Step 4: Verify the hook runs green**

Run: `.githooks/pre-commit`
Expected: prints the four `pre-commit:` lines ending in `pre-commit: OK`, exit 0.

- [ ] **Step 5: Commit**

```bash
git add justfile .githooks/pre-commit
git commit -m "build: Add justfile and POSIX pre-commit hook (fmt, clippy, headers)"
```

---

### Task 9: CI — quality + MSRV jobs

**Files:**
- Modify: `.github/workflows/ci.yml` (add two jobs under `jobs:`)

- [ ] **Step 1: Add the `quality` job**

Append under `jobs:` in `.github/workflows/ci.yml` (sibling to `rust-checks`):
```yaml
  quality:
    name: Quality gates
    runs-on: ubuntu-22.04
    steps:
      - uses: actions/checkout@v6

      - name: Install Rust
        uses: dtolnay/rust-toolchain@stable

      - name: Cache Rust
        uses: Swatinem/rust-cache@v2

      - name: License header check
        run: ./scripts/check-headers.sh

      - name: Rustdoc (deny broken links)
        run: RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features

      - name: cargo-deny
        uses: EmbarkStudios/cargo-deny-action@v2
        with:
          command: check
```

- [ ] **Step 2: Add the `msrv` job (reads the floor from Cargo.toml — single source of truth)**

Append under `jobs:`:
```yaml
  msrv:
    name: MSRV build
    runs-on: ubuntu-22.04
    steps:
      - uses: actions/checkout@v6

      - name: Read MSRV from Cargo.toml
        id: msrv
        run: echo "version=$(grep '^rust-version' Cargo.toml | sed 's/.*= *"\(.*\)"/\1/')" >> "$GITHUB_OUTPUT"

      - name: Install pinned MSRV toolchain
        uses: dtolnay/rust-toolchain@master
        with:
          toolchain: ${{ steps.msrv.outputs.version }}

      - name: Cache Rust
        uses: Swatinem/rust-cache@v2

      - name: Build + test on MSRV
        run: cargo test --all-targets --all-features
```

- [ ] **Step 3: Validate the workflow YAML locally**

Run: `python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/ci.yml')); print('valid yaml')"`
Expected: `valid yaml`, exit 0.

- [ ] **Step 4: Re-run the full local acceptance set (the checks CI will run)**

Run:
```bash
cargo fmt --check \
 && cargo clippy --all-targets --all-features -- -D warnings \
 && cargo test --all-targets --all-features \
 && RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features \
 && cargo deny check \
 && ./scripts/check-headers.sh \
 && echo "ALL GATES PASS"
```
Expected: `ALL GATES PASS`, exit 0.

- [ ] **Step 5: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: Add quality (deny, headers, rustdoc) and MSRV jobs"
```

---

### Task 10: Document the toolchain in CONTRIBUTING.md

**Files:**
- Modify: `CONTRIBUTING.md` (add a "Local development gates" section)

- [ ] **Step 1: Append a section to `CONTRIBUTING.md`**

```markdown
## Local development gates

This repo enforces formatting, linting, documentation, license headers, and
supply-chain policy. To run the same checks locally:

- One-time setup (installs the git hooks): `just setup`
  (or `git config core.hooksPath .githooks`)
- Fast gate (runs automatically on commit): `just check`
  — `cargo fmt --check`, `cargo clippy ... -D warnings`, `./scripts/check-headers.sh`
- Full gate (run before pushing): `just test`, `just deny`, `just doc`

Tooling required: `rustfmt` + `clippy` (rustup components), and for the full gate
`cargo install cargo-deny`. `just` is optional — the hook and CI call `cargo` directly.

Every `.rs` file must begin with:

    // SPDX-License-Identifier: MIT
    // Copyright (c) 2026 James Maes

All public and private items must carry doc comments — `missing_docs` and
`clippy::missing_docs_in_private_items` are denied.
```

- [ ] **Step 2: Commit**

```bash
git add CONTRIBUTING.md
git commit -m "docs: Document local development gates and toolchain in CONTRIBUTING"
```

---

## Self-review notes

- **Spec coverage:** §1 doc-lints → Task 4; §2 headers → Tasks 2–3; §3 MSRV → Tasks 6 & 9; §4 cargo-deny → Tasks 7 & 9; §5 hooks/justfile → Task 8; §6 CI quality+MSRV → Task 9; §7 rustfmt.toml → Task 5; rustdoc gate → Task 9. CONTRIBUTING (impl detail) → Task 10. All spec sections mapped.
- **Ordering:** local artifacts (header script, headers, lints, rustfmt, MSRV, deny.toml, hooks) precede the CI that references them (Task 9). Doc baseline (Task 1) precedes deny-lint escalation (Task 4).
- **Derived value:** the MSRV literal is produced by `cargo-msrv` in Task 6 and consumed in Task 9 via a Cargo.toml read (no hardcoded duplication, no placeholder in the shipped config).
- **Caveat carried from spec:** `tests/` private-item docs are not clippy-enforceable; only rustc `missing_docs` (public items) applies there. No task attempts the impossible.
