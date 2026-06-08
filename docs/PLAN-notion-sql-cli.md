# PLAN: notion-sql CLI

## Goal
Build a publish-ready Rust CLI that parses SQL-style CRUD statements and translates them into typed Notion REST API calls.

## Approach
Create a binary crate with a small modular core:

- SQL parser and statement mapper using `sqlparser`
- Notion schema model and case-insensitive property lookup
- Filter translation from SQL expressions into Notion filter JSON
- Value coercion from SQL literals into typed Notion property payloads
- Blocking Notion HTTP client with pagination and 429 retry handling
- CLI presentation for table, JSON, CSV, dry-run summaries, and `--apply`

Mutating operations stay dry-run by default. Tests focus on filter translation and value coercion because those areas carry the most behavioral risk.

## Files Affected
- `Cargo.toml` - crate metadata, dependencies, and dist config
- `src/main.rs` - CLI entrypoint
- `src/lib.rs` - module exports
- `src/cli.rs` - clap arguments and command dispatch
- `src/sql.rs` - SQL parsing and supported statement extraction
- `src/schema.rs` - Notion database schema model and lookup helpers
- `src/filter.rs` - WHERE expression to Notion filter translation
- `src/value.rs` - SQL literal to typed Notion property value coercion
- `src/notion.rs` - Notion REST client, pagination, retries, schema resolution
- `src/output.rs` - table, JSON, CSV rendering and mutation summaries
- `README.md` - setup, auth, examples, install notes
- `RELEASING.md` - cargo-dist and Homebrew release flow
- `.github/workflows/release.yml` - generated-style dist release workflow

## Steps
1. [ ] Scaffold Rust binary crate and metadata.
2. [ ] Add planning/session continuity docs.
3. [ ] Implement SQL statement parsing for SELECT, INSERT, UPDATE, DELETE.
4. [ ] Implement schema lookup, property type modeling, and value coercion.
5. [ ] Implement WHERE filter translation and focused unit tests.
6. [ ] Implement Notion HTTP client with name resolution, pagination, and retry.
7. [ ] Implement CLI execution, dry-run/apply behavior, and output formats.
8. [ ] Add README, RELEASING, and dist/Homebrew configuration.
9. [ ] Run `cargo fmt`, `cargo test`, and `cargo build --release`.

## Open Questions
- No GitHub issue is associated with this work by user direction.
- Homebrew tap owner is `KofTwentyTwo`; release automation targets `KofTwentyTwo/homebrew-tap`.
- `HOMEBREW_TAP_TOKEN` must be added to `KofTwentyTwo/notion-sql` before the first public release tag.
