# notion-sql

`notion-sql` is a Rust CLI for running SQL-style statements against Notion databases through the official Notion REST API.

It parses real SQL with `sqlparser`, introspects the target Notion database schema, and translates supported CRUD statements into typed Notion API calls.

## Install

Homebrew releases are configured through `cargo-dist`:

```bash
brew install KofTwentyTwo/tap/notion-sql
```

From source:

```bash
cargo install --path .
```

## Authentication

Create a Notion internal integration, copy its integration secret, and expose it as `NOTION_TOKEN`:

```bash
export NOTION_TOKEN="ntn_..."
```

Share each target Notion database with that integration. Notion only returns databases and pages that the integration can access.

## Notion API Version

`v0.1.0` intentionally pins `Notion-Version: 2022-06-28` and uses Notion's legacy database endpoints. This keeps the first release focused on the existing database query, page create, page update, and page trash APIs.

Notion's `2025-09-03` API introduces data sources and deprecates the older database query endpoint for clients that opt into that version. Databases with multiple data sources may not work with `v0.1.0`; migration to data source endpoints is planned for a later release.

## Database Names And IDs

The SQL table name can be either:

- A Notion database ID, with or without dashes.
- A friendly database name.

Friendly names are resolved with the Notion search endpoint. If no database matches, or multiple databases match, `notion-sql` exits with a clear candidate list.

## Usage

```bash
notion-sql --list-databases
notion-sql "SELECT Name, Status FROM Tasks WHERE Status='Done'"
notion-sql "SELECT COUNT(*) FROM Tasks WHERE Status='Done'"
notion-sql "SELECT * FROM Tasks WHERE Priority >= 2 ORDER BY Due ASC LIMIT 10"
notion-sql "INSERT INTO Tasks (Name, Status) VALUES ('New task', 'To Do')" --apply
notion-sql "UPDATE Tasks SET Status='Archived' WHERE Priority='Low'" --apply
notion-sql "DELETE FROM Tasks WHERE Status='Done'" --apply
notion-sql "DELETE FROM Tasks WHERE Status='Done'" --apply --progress
```

Mutating statements are dry-run by default. Add `--apply` to write changes.

Applied `UPDATE` and `DELETE` statements require a `WHERE` clause unless `--force-all` is passed. Use `--force-all` only for intentional full-database mutations.

Use `--progress` for long-running queries or mutations. Progress is written to stderr so stdout remains usable for tables, JSON, and CSV.

Output formats for `SELECT`:

```bash
notion-sql "SELECT Name, Status FROM Tasks" --json
notion-sql "SELECT Name, Status FROM Tasks" --csv
notion-sql --list-databases --json
```

## Supported SQL

Supported statements:

- `SELECT <cols|*> FROM <db> [WHERE ...] [ORDER BY col [ASC|DESC]] [LIMIT n]`
- `SELECT COUNT(*) FROM <db> [WHERE ...]`
- `INSERT INTO <db> (col1, col2) VALUES (v1, v2), ...`
- `UPDATE <db> SET col=val[, col2=val2 ...] [WHERE ...]`
- `DELETE FROM <db> [WHERE ...]`

Supported `WHERE` operators:

- `=`, `!=`, `>`, `<`, `>=`, `<=`
- `LIKE`, mapped to `equals`, `starts_with`, `ends_with`, or `contains` for supported wildcard shapes
- `IN (...)`, mapped to a Notion `or` filter
- `IS NULL`, `IS NOT NULL`
- Nested `AND` and `OR`

Supported Notion property types:

- `title`
- `rich_text`
- `select`
- `status`
- `multi_select`
- `number`
- `checkbox`
- `date`

Unsupported SQL features fail clearly rather than being guessed. Examples: joins, aggregates beyond `COUNT(*)`, group-by, subqueries, aliases, arbitrary expressions in projections, unsupported `LIKE` wildcard shapes, and unsupported Notion property types in `SELECT *`.

## Development

```bash
cargo fmt
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo build --release
```

The risky code paths are covered by unit tests:

- SQL statement extraction from `sqlparser` ASTs.
- SQL `WHERE` expression translation to Notion filters.
- SQL literal coercion into typed Notion property payloads.

## Release Automation

Release automation is handled by `dist` 0.32.0.

- Branch pushes and PRs run CI.
- Pushes to `develop`, `release/*`, `rc/*`, and `main` also upload branch build artifacts.
- Version tags matching `vX.Y.Z` create production GitHub Releases and publish the Homebrew formula.
- Version tags matching `vX.Y.Z-rc.N` create GitHub prereleases for release candidates.
- Homebrew formulas publish to `KofTwentyTwo/homebrew-tap`, which requires the `HOMEBREW_TAP_TOKEN` repository secret.

See [RELEASING.md](RELEASING.md) for the full flow.
