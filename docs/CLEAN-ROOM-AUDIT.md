# Clean-Room Audit

Date: June 2, 2026

Scope: full pre-release audit of code, architecture, SQL behavior, Notion API integration, safety controls, tests, documentation, comments, build system, GitHub Actions, release automation, and public repository readiness.

Verdict after remediation: source code, documentation, build checks, release planning, local package dry-runs, Git remote reachability, Homebrew tap visibility, and the `HOMEBREW_TAP_TOKEN` repository secret are release-ready.

## Verification

| Gate | Result |
|---|---|
| `cargo fmt --check` | Pass |
| `cargo clippy --all-targets --all-features -- -D warnings` | Pass |
| `cargo test --all-targets --all-features` | Pass, 34 tests |
| `RUSTDOCFLAGS='-D warnings' cargo doc --no-deps --all-features` | Pass |
| `cargo build --release` | Pass |
| `cargo package --allow-dirty` | Pass |
| `cargo publish --dry-run --allow-dirty` | Pass |
| `nix run nixpkgs#actionlint -- .github/workflows/ci.yml .github/workflows/release.yml` | Pass |
| `dist generate --check` | Pass |
| `dist plan --tag v0.1.0` | Pass |
| `cargo run -- --help` | Pass |
| `git ls-remote --heads origin` | Pass, remote currently has `develop` |
| `gh repo view KofTwentyTwo/homebrew-tap` | Pass, public repo exists |
| `gh secret list --repo KofTwentyTwo/notion-sql` | Pass, `HOMEBREW_TAP_TOKEN` listed |

## Remediated Findings

### 1. Homebrew publishing placeholders

Status: Remediated in code and docs.

Changes:

1. `Cargo.toml` now uses `tap = "KofTwentyTwo/homebrew-tap"`.
2. `.github/workflows/release.yml` now checks out `KofTwentyTwo/homebrew-tap`.
3. `README.md` uses `brew install KofTwentyTwo/tap/notion-sql`.
4. `RELEASING.md` documents tap setup and `HOMEBREW_TAP_TOKEN`.

External check still required: verify the tap exists, is public, and the token has contents write access.

### 2. Case-insensitive property collisions

Status: Remediated.

`DatabaseSchema::from_notion_database` now rejects case-insensitive property-name collisions such as `Status` and `status` with a clear ambiguity error. Tests cover the collision path.

### 3. Applied full-table UPDATE and DELETE

Status: Remediated.

Applied `UPDATE` and `DELETE` now require a `WHERE` clause unless `--force-all` is passed. Dry-runs still allow no-`WHERE` statements for inspection. Tests cover the guard.

### 4. LIKE semantics

Status: Remediated.

Supported `LIKE` shapes now map to Notion filters:

- `foo` to `equals`
- `foo%` to `starts_with`
- `%foo` to `ends_with`
- `%foo%` to `contains`
- supported `NOT LIKE` shapes to `does_not_equal` or `does_not_contain`

Unsupported internal `%`, `_`, escaped wildcard, wildcard-only, and negative prefix/suffix shapes fail clearly. Tests cover supported and rejected patterns.

### 5. Long-running progress

Status: Remediated.

`--progress` now writes query and row mutation progress to stderr. This keeps stdout usable for tables, JSON, and CSV while giving visibility during multi-minute Notion operations.

### 6. Friendly database resolution pagination

Status: Remediated.

Database name resolution now paginates all `/v1/search` results before deciding zero, one, or many exact matches. Tests cover matches and duplicates across pages.

### 7. HTTP timeout and retry cap

Status: Remediated.

The Notion client now has a 30 second request timeout and caps individual `Retry-After` sleeps. Tests cover retry behavior and max retry exhaustion without slow real sleeps.

### 8. NULL clearing

Status: Remediated for nullable writable property types.

`NULL` can now clear rich text, number, select, multi-select, and date properties. Title, checkbox, and status clearing are intentionally rejected.

### 9. Date output

Status: Remediated.

Date display now preserves start/end ranges and time zone metadata when present.

### 10. Unsupported property types in SELECT star

Status: Remediated.

`SELECT *` now rejects unsupported Notion property types with a clear error instead of rendering fallback JSON-like strings.

### 11. Public repository ignores

Status: Remediated.

`.gitignore` now covers Rust output, cargo-dist artifacts, env files, OS/editor state, logs, temp files, and local agent/tooling state while preserving example env files.

### 12. Mocked Notion API coverage

Status: Remediated.

In-file TCP mock tests cover paginated search, duplicate resolution, rate-limit retry behavior, mutation request paths/headers/payloads, and structured error handling. No extra dev dependency was needed.

### 13. Notion API version documentation

Status: Documented for `v0.1.0`.

`README.md` and `RELEASING.md` now state that `v0.1.0` intentionally targets `Notion-Version: 2022-06-28` and legacy database endpoints. Migration to `2025-09-03` data source endpoints remains deferred.

Official references:

- https://developers.notion.com/guides/get-started/upgrade-guide-2025-09-03
- https://developers.notion.com/reference/post-database-query

## Remaining Operational Items

None known before `v0.1.0` tagging.

## Final Release Gate

Run this exact gate immediately before pushing `main` and tagging:

1. `cargo fmt --check`
2. `cargo clippy --all-targets --all-features -- -D warnings`
3. `cargo test --all-targets --all-features`
4. `RUSTDOCFLAGS='-D warnings' cargo doc --no-deps --all-features`
5. `cargo build --release`
6. `cargo publish --dry-run --allow-dirty`
7. `nix run nixpkgs#actionlint -- .github/workflows/ci.yml .github/workflows/release.yml`
8. `dist generate --check`
9. `dist plan --tag v0.1.0`
10. `git ls-remote --heads origin`
11. `env -u GITHUB_TOKEN gh secret list --repo KofTwentyTwo/notion-sql`

Manual Notion smoke tests against a throwaway database:

1. `notion-sql --list-databases`
2. `notion-sql "SELECT COUNT(*) FROM \"Task List\""`
3. `notion-sql "SELECT Name, Status FROM \"Task List\" LIMIT 5" --progress`
4. One dry-run `INSERT`, `UPDATE`, and `DELETE`
5. One applied `INSERT` against a throwaway test database
6. One applied `DELETE` against the throwaway row with `--progress`
