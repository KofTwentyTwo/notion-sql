// SPDX-License-Identifier: MIT
// Copyright (c) 2026 James Maes
//! Integration tests for [`notion_sql::schema::DatabaseSchema`] parsing.
//!
//! These tests exercise the public surface of `DatabaseSchema` from outside
//! the crate (this is a `tests/` integration test, so it sees only the public
//! API), focusing on two safety/usability invariants of the conversion from a
//! raw Notion database JSON payload into our internal schema:
//!
//! * **Case-insensitive name collisions are rejected.** SQL identifiers are
//!   matched case-insensitively, but Notion allows two properties whose names
//!   differ only by case (e.g. `Status` vs `status`). Such a pair cannot be
//!   disambiguated in a SQL query, so `from_notion_database` must surface a
//!   clear error rather than silently picking one.
//! * **Unsupported property types are reported, not dropped.** Some Notion
//!   property types (e.g. `formula`) have no SQL-queryable representation. The
//!   schema must still construct successfully but expose these columns via
//!   `unsupported_columns()` so callers can inform the user.
//!
//! The approach is black-box: each test feeds a hand-built `serde_json` value
//! that mimics the shape Notion's API returns, then asserts on the resulting
//! `Result`/value. Building JSON inline keeps each test self-contained and
//! makes the precise property shape under test obvious at a glance.

use notion_sql::schema::DatabaseSchema;
use serde_json::json;

/// Verifies that two property names differing only in letter case are rejected.
///
/// Notion permits `Status` and `status` to coexist as distinct properties, but
/// our SQL layer resolves column names case-insensitively, so the pair is
/// inherently ambiguous. This test confirms `from_notion_database` returns an
/// `Err` (via `unwrap_err`) and that the rendered message both names the failure
/// mode ("Ambiguous Notion property names") and includes *both* offending names
/// so the user can see exactly which properties clash.
///
/// # Panics
///
/// Panics (failing the test) if construction unexpectedly succeeds, or if the
/// error message omits any of the three expected substrings.
#[test]
fn rejects_case_insensitive_property_name_collisions() {
    // Two properties whose names collide only on case; the differing `type`
    // values prove the rejection is driven by name, not by property kind.
    let error = DatabaseSchema::from_notion_database(&json!({
        "properties": {
            "Status": { "type": "status", "status": {} },
            "status": { "type": "select", "select": {} }
        }
    }))
    .unwrap_err()
    .to_string();

    // Message must identify the failure class and list each clashing name so
    // the user can locate and rename the offending properties.
    assert!(error.contains("Ambiguous Notion property names"));
    assert!(error.contains("Status"));
    assert!(error.contains("status"));
}

/// Verifies that unsupported property types are reported rather than discarded.
///
/// Construction must succeed even when a property type (here `formula`) has no
/// SQL-queryable mapping; the supported `title` column ("Name") keeps the schema
/// valid. The unsupported column is then surfaced through `unsupported_columns()`
/// in a `"<name> (<type>)"` display form so callers can warn the user about
/// which columns will be missing from queries.
///
/// # Panics
///
/// Panics (failing the test) if construction returns `Err`, or if
/// `unsupported_columns()` does not return exactly the expected single entry —
/// confirming the supported `title` column is excluded from the list.
#[test]
fn lists_unsupported_columns() {
    // "Name" is a supported `title` property; "Formula" is the unsupported case
    // we expect to be reported back to the caller.
    let schema = DatabaseSchema::from_notion_database(&json!({
        "properties": {
            "Name": { "type": "title", "title": {} },
            "Formula": { "type": "formula", "formula": {} }
        }
    }))
    .unwrap();

    // Only the formula column should appear, rendered as "<name> (<type>)".
    assert_eq!(
        schema.unsupported_columns(),
        vec!["Formula (formula)".to_string()]
    );
}
