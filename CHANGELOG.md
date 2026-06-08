# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.0.1] - 2026-06-08

Maintenance release: documentation and development-tooling hardening only. No
runtime behavior changes.

### Added
- Comprehensive doc comments across all source and test files (module-level `//!`
  headers and `///` docs on every public and private item).
- Enforced documentation lints: `missing_docs` and
  `clippy::missing_docs_in_private_items` set to `deny` via a `Cargo.toml [lints]` table.
- SPDX license + copyright headers on every Rust source/test file, enforced by
  `scripts/check-headers.sh`.
- `deny.toml` supply-chain policy (cargo-deny: advisories, license allowlist, bans,
  sources) gated by a CI `quality` job.
- Declared MSRV (`rust-version = "1.88"`) with a dedicated CI job that builds and
  tests on the pinned toolchain.
- `rustfmt.toml` (edition pin), a `justfile`, and a POSIX `pre-commit` git hook
  running fmt / clippy / header checks locally.
- CI rustdoc gate (`RUSTDOCFLAGS="-D warnings"`) to catch broken intra-doc links.

### Changed
- Documented local development gates and toolchain in `CONTRIBUTING.md`.

## [1.0.0] - 2026-06-02

### Added
- SQL SELECT with WHERE, ORDER BY, LIMIT support
- SQL INSERT, UPDATE, DELETE with dry-run mode by default
- Database resolution by ID or friendly name via Notion search API
- JSON and CSV output formats for SELECT queries
- Full support for 8 Notion property types (title, rich_text, select, status, multi_select, number, checkbox, date)
- Comprehensive WHERE operators (=, !=, >, <, >=, <=, LIKE, IN, IS NULL/IS NOT NULL)
- Nested AND/OR logic in WHERE clauses
- Automatic pagination for large database queries
- Rate limiting with automatic retry (5 attempts)
- Progress reporting for long-running operations

### Changed
- None

### Fixed
- None

### Breaking Changes
- None

---

## [0.1.0] - 2026-06-02

### Added
- Initial release of notion-sql CLI
- Core SQL parsing and translation to Notion API calls
- Basic SELECT, INSERT, UPDATE, DELETE operations
- Table output format for SELECT queries
