// SPDX-License-Identifier: MIT
// Copyright (c) 2026 James Maes
//! Integration tests for the notion-sql crate.
//!
//! These tests require network access to the Notion API and are marked
//! with the `#[ignore]` attribute by default. Run them with:
//!
//! ```text
//! cargo test --ignored
//! ```
//!
//! # Purpose and approach
//!
//! This file exercises the crate's two public-facing entry points end to end,
//! treating the crate as an external consumer would (i.e. it only imports the
//! re-exported public API, never internal modules). It is deliberately
//! organized around two concerns:
//!
//! * Schema translation — turning a Notion database JSON payload (the shape the
//!   Notion REST API returns) into the crate's [`DatabaseSchema`] type, and
//!   confirming the discovered column names match the source properties.
//! * SQL parsing — confirming that each of the four supported statement kinds
//!   (`SELECT`, `INSERT`, `UPDATE`, `DELETE`) is accepted by
//!   [`parse_statement`]. These tests assert only that parsing *succeeds*;
//!   semantic validation against a live database belongs to the network-gated
//!   tests described above and is intentionally out of scope here.
//!
//! Note that despite the module header's mention of network access, every test
//! currently in this file is hermetic — they run against in-memory JSON and SQL
//! strings and require no Notion credentials, which is why none carry
//! `#[ignore]`. The header documents the broader intent for this file as the
//! home for live integration coverage as it is added.

use notion_sql::schema::DatabaseSchema;
use notion_sql::sql::parse_statement;
use serde_json::json;

/// Verifies that a Notion database JSON payload survives translation into a
/// [`DatabaseSchema`] with all of its property names preserved as columns.
///
/// The "round trip" here is JSON properties in, column names out: it guards
/// against regressions where a property type fails to map to a column or its
/// name is dropped/altered during parsing. The two property types chosen
/// (`title` and `status`) are representative of the distinct shapes Notion
/// uses, so the test also implicitly covers handling more than one property
/// kind in a single schema.
///
/// # Panics
///
/// Panics (failing the test) if [`DatabaseSchema::from_notion_database`]
/// returns `Err` — via `unwrap` — or if either expected column is missing from
/// the discovered set.
#[test]
fn database_schema_round_trip() {
    // Hand-build the minimal slice of Notion's database object that the parser
    // cares about: the `properties` map keyed by property name. Each property
    // carries a `type` discriminant plus a same-named config object — this
    // mirrors Notion's actual API response shape, where the empty `{}` config
    // is valid for these property kinds.
    let json_db = json!({
        "properties": {
            "Name": { "type": "title", "title": {} },
            "Status": { "type": "status", "status": {} }
        }
    });

    // Unwrap is intentional: a parse failure on this well-formed input is a
    // genuine regression and should fail the test loudly.
    let schema = DatabaseSchema::from_notion_database(&json_db).unwrap();
    let columns: Vec<String> = schema.available_columns();

    // Column ordering is not guaranteed by the schema, so assert membership
    // rather than positional equality.
    assert!(columns.contains(&"Name".to_string()));
    assert!(columns.contains(&"Status".to_string()));
}

/// Confirms the parser accepts a representative `SELECT` with a projection list
/// and a `WHERE` clause containing a string-literal comparison.
///
/// Asserts success only; the parsed AST is not inspected here.
///
/// # Panics
///
/// Panics (failing the test) if [`parse_statement`] returns `Err`.
#[test]
fn parse_select_statement() {
    let sql = "SELECT Name, Status FROM database WHERE Status = 'Active'";
    let parsed = parse_statement(sql);

    assert!(parsed.is_ok(), "Failed to parse SELECT statement");
}

/// Confirms the parser accepts an `INSERT` with an explicit column list and a
/// matching `VALUES` tuple of string literals.
///
/// Asserts success only; the parsed AST is not inspected here.
///
/// # Panics
///
/// Panics (failing the test) if [`parse_statement`] returns `Err`.
#[test]
fn parse_insert_statement() {
    let sql = "INSERT INTO database (Name, Status) VALUES ('Test', 'Active')";
    let parsed = parse_statement(sql);

    assert!(parsed.is_ok(), "Failed to parse INSERT statement");
}

/// Confirms the parser accepts an `UPDATE` with a `SET` assignment and a
/// `WHERE` clause that targets a single row by name.
///
/// Asserts success only; the parsed AST is not inspected here.
///
/// # Panics
///
/// Panics (failing the test) if [`parse_statement`] returns `Err`.
#[test]
fn parse_update_statement() {
    let sql = "UPDATE database SET Status = 'Done' WHERE Name = 'Task 1'";
    let parsed = parse_statement(sql);

    assert!(parsed.is_ok(), "Failed to parse UPDATE statement");
}

/// Confirms the parser accepts a `DELETE` qualified by a `WHERE` clause.
///
/// Asserts success only; the parsed AST is not inspected here. The `WHERE`
/// clause is included deliberately — an unqualified `DELETE` would target every
/// row, which is not the shape this test means to cover.
///
/// # Panics
///
/// Panics (failing the test) if [`parse_statement`] returns `Err`.
#[test]
fn parse_delete_statement() {
    let sql = "DELETE FROM database WHERE Status = 'Archived'";
    let parsed = parse_statement(sql);

    assert!(parsed.is_ok(), "Failed to parse DELETE statement");
}
