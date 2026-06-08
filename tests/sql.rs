// SPDX-License-Identifier: MIT
// Copyright (c) 2026 James Maes
//! Integration tests for the crate's SQL front end.
//!
//! These tests exercise [`notion_sql::sql::parse_statement`], the public entry
//! point that turns a raw SQL string into the crate's own [`SqlStatement`]
//! model (the in-memory representation that later layers translate into Notion
//! API calls). The goal here is to pin down the *parsing contract* — which SQL
//! shapes are accepted and what structured output they produce — independently
//! of any Notion connectivity, so these run fast and offline.
//!
//! Approach: each test feeds one representative statement to the parser and
//! then `match`es on the resulting [`SqlStatement`] variant. We assert on the
//! individual fields we care about (database name, projected columns, sort
//! order, limit, inserted rows) rather than constructing a whole expected
//! `SqlStatement` value; this keeps a test focused on the behavior it owns and
//! avoids brittle coupling to fields it does not care about (hence the `..`
//! rest patterns). The non-matching arm `panic!`s with the unexpected variant
//! so a regression that changes the parsed shape fails loudly and legibly.
//!
//! Coverage spans both the happy paths (`SELECT`, `INSERT`, `COUNT(*)`) and one
//! deliberately rejected case (`COUNT(<column>)`), so the suite documents both
//! what the parser supports and where it draws its limits.
//!
//! As an integration test (in `tests/`), this file may only touch the crate's
//! public API surface, which is why everything is reached through the
//! `notion_sql::sql` module path.

use notion_sql::sql::{parse_statement, SelectColumns, SqlStatement};

/// Verifies that a full-featured `SELECT` is parsed into a `SqlStatement::Select`
/// with every clause decoded correctly.
///
/// The input combines a projection list, a `WHERE` filter, an `ORDER BY ...
/// DESC`, and a `LIMIT`, so this is the broad "does the SELECT grammar hang
/// together" check. We assert that:
/// - the source `database` resolves to the `FROM` target (`Tasks`),
/// - the projection becomes `SelectColumns::Columns` preserving column order,
/// - the `DESC` sort is captured as a `SortSpec` with `ascending: false`,
/// - the `LIMIT 5` surfaces as `Some(5)`.
///
/// The `WHERE` clause is intentionally *not* asserted on here (it is covered
/// only as part of accepting the statement), hence the `..` in the pattern.
///
/// # Panics
///
/// Panics via `unwrap()` if the statement fails to parse, and via the catch-all
/// `panic!` arm if the parser returns any variant other than `Select`.
#[test]
fn parses_select_statement() {
    let parsed = parse_statement(
        "SELECT Name, Status FROM Tasks WHERE Status = 'Done' ORDER BY Name DESC LIMIT 5",
    )
    .unwrap();

    match parsed {
        // Destructure only the clauses under test; `..` ignores the parsed
        // `WHERE` filter and any other fields so this test stays insulated from
        // unrelated additions to the `Select` variant.
        SqlStatement::Select {
            database,
            columns,
            sorts,
            limit,
            ..
        } => {
            assert_eq!(database, "Tasks");
            assert_eq!(
                columns,
                SelectColumns::Columns(vec!["Name".to_string(), "Status".to_string()])
            );
            // `ORDER BY Name DESC` must yield exactly one sort key whose
            // `ascending` flag is `false`; this guards the DESC -> false mapping.
            assert_eq!(
                sorts,
                vec![notion_sql::sql::SortSpec {
                    column: "Name".to_string(),
                    ascending: false
                }]
            );
            assert_eq!(limit, Some(5));
        }
        // Any non-`Select` result is a parser regression; surface the actual
        // variant in the failure message for quick diagnosis.
        other => panic!("unexpected statement: {other:?}"),
    }
}

/// Verifies that an `INSERT ... VALUES` is parsed into a `SqlStatement::Insert`
/// with the target table, column list, and row count decoded correctly.
///
/// The input inserts a single row of two columns. We assert that:
/// - the target `database` is `Tasks`,
/// - the explicit `columns` list is preserved in order,
/// - exactly one row was parsed (`rows.len() == 1`).
///
/// The individual cell *values* are not asserted here — only the row arity — to
/// keep the test focused on statement shape rather than value coercion.
///
/// # Panics
///
/// Panics via `unwrap()` if parsing fails, and via the catch-all `panic!` arm
/// if the parser returns any variant other than `Insert`.
#[test]
fn parses_insert_values() {
    let parsed =
        parse_statement("INSERT INTO Tasks (Name, Status) VALUES ('New task', 'To Do')").unwrap();

    match parsed {
        SqlStatement::Insert {
            database,
            columns,
            rows,
        } => {
            assert_eq!(database, "Tasks");
            assert_eq!(columns, vec!["Name".to_string(), "Status".to_string()]);
            // One `VALUES (...)` tuple was supplied, so exactly one row is
            // expected; this also asserts the parser does not split or merge
            // rows.
            assert_eq!(rows.len(), 1);
        }
        other => panic!("unexpected statement: {other:?}"),
    }
}

/// Verifies that `SELECT COUNT(*)` is recognized as the dedicated count
/// projection rather than treated as a regular column list.
///
/// `COUNT(*)` is special-cased into `SelectColumns::Count` because Notion has no
/// generic aggregate facility — the crate satisfies it by counting returned
/// pages — so the parser must distinguish it from an ordinary projection. We
/// assert the `database` is `Tasks` and that `columns` is exactly
/// `SelectColumns::Count`. The `WHERE` clause is accepted but not asserted on.
///
/// # Panics
///
/// Panics via `unwrap()` if parsing fails, and via the catch-all `panic!` arm
/// if the parser returns any variant other than `Select`.
#[test]
fn parses_count_projection() {
    let parsed = parse_statement("SELECT COUNT(*) FROM Tasks WHERE Status = 'Done'").unwrap();

    match parsed {
        SqlStatement::Select {
            database, columns, ..
        } => {
            assert_eq!(database, "Tasks");
            assert_eq!(columns, SelectColumns::Count);
        }
        other => panic!("unexpected statement: {other:?}"),
    }
}

/// Verifies that counting a specific column (`COUNT(Name)`) is rejected, pinning
/// down the limit of the parser's `COUNT` support.
///
/// Only `COUNT(*)` and `COUNT(1)` map cleanly onto "number of pages returned";
/// `COUNT(<column>)` would imply non-null counting semantics the crate cannot
/// honor against Notion, so it must error rather than silently mis-parse. We
/// take the `Err`, render it with `to_string()`, and assert the message
/// contains the substring `"Only COUNT(*) and COUNT(1)"` — a substring match so
/// the test tolerates surrounding wording while still proving the user is told
/// *which* forms are allowed.
///
/// # Panics
///
/// Panics via `unwrap_err()` if the statement unexpectedly parses successfully,
/// and via `assert!` if the error message does not mention the supported forms.
#[test]
fn rejects_unsupported_count_projection() {
    // `unwrap_err` asserts the parse fails; a successful parse here would be the
    // bug this test is designed to catch.
    let error = parse_statement("SELECT COUNT(Name) FROM Tasks").unwrap_err();

    assert!(error.to_string().contains("Only COUNT(*) and COUNT(1)"));
}
