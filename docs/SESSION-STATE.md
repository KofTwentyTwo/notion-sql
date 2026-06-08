# Session State

## Current Task
Build `notion-sql`, a Rust CLI for running SQL-style CRUD statements against Notion databases through the Notion REST API.

## Branch
`feature/no-issue-notion-sql-cli`

## Ticket
None, per user direction.

## Important Decisions
- Parse real SQL using `sqlparser`; do not hand-write SQL parsing.
- Resolve Notion databases by ID or friendly name.
- Introspect database schema before translating filters or write payloads.
- Mutating statements default to dry-run and require `--apply`.
- Use `NOTION_TOKEN` for auth and `Notion-Version: 2022-06-28`.
- Homebrew tap owner is `KofTwentyTwo`; release config targets `KofTwentyTwo/homebrew-tap`.
- `HOMEBREW_TAP_TOKEN` is present on `KofTwentyTwo/notion-sql` Actions secrets.

## Verification Plan
- `cargo fmt`
- `cargo test`
- `cargo build --release`

## Current Verification Status
- `cargo fmt --check` passed.
- `cargo clippy --all-targets --all-features -- -D warnings` passed.
- `cargo test --all-targets --all-features` passed with 34 tests.
- `cargo build --release` passed.
- `nix run nixpkgs#actionlint -- .github/workflows/ci.yml .github/workflows/release.yml` passed.
- `dist generate --check` passed.
- `dist plan --tag v0.1.0` passed and includes shell installer, Homebrew formula, checksums, and the four configured target archives.
- `git ls-remote --heads origin` passed after earlier SSH-agent failure.
- `gh repo view KofTwentyTwo/homebrew-tap` confirmed the tap exists and is public.
- `gh secret list --repo KofTwentyTwo/notion-sql` lists `HOMEBREW_TAP_TOKEN`.
