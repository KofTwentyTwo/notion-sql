// SPDX-License-Identifier: MIT
// Copyright (c) 2026 James Maes
//! Integration tests for CLI-level safety checks that run entirely offline.
//!
//! # Purpose
//! This test binary exercises the parts of the CLI surface that protect users
//! from destructive or unsupported operations *before* any network round-trip
//! to Notion is attempted. By construction every test here is hermetic: it
//! depends only on pure functions and in-memory schema parsing, so it requires
//! no API token, no live database, and no HTTP mocking. That keeps the suite
//! fast and deterministic in CI.
//!
//! # Behavior under test
//! - [`guard_applied_full_table_mutation`]: the guard rail that refuses to apply
//!   a row-affecting statement (e.g. `DELETE`/`UPDATE`) to an entire table
//!   unless the user has narrowed it with a `WHERE` clause or has explicitly
//!   opted into the blast radius via `--force-all`. Both the rejection path and
//!   the three "this is allowed" paths are covered.
//! - [`DatabaseSchema::unsupported_columns`]: detection of Notion property types
//!   that this tool cannot faithfully represent (e.g. `formula`), which the CLI
//!   uses to warn or bail when a `SELECT *` would otherwise silently drop data.
//!
//! # Testing approach
//! The tests treat the crate as an external consumer would: they import only
//! the crate's public API (`notion_sql::cli` / `notion_sql::schema`) rather than
//! reaching into private internals. Notion API payloads are simulated with
//! hand-written `serde_json` literals so schema parsing can be validated without
//! a real database fixture.

use notion_sql::{cli::guard_applied_full_table_mutation, schema::DatabaseSchema};
use serde_json::json;

/// Verifies the guard rejects an *applied* full-table mutation that has neither
/// a `WHERE` clause nor the explicit `--force-all` override.
///
/// This is the dangerous combination the guard exists to stop: `apply = true`
/// (the change is real, not a dry run), `force_all = false` (the user has not
/// opted into affecting every row), and `has_filter = false` (no `WHERE` to
/// narrow scope). The assertion checks the error message specifically mentions
/// the missing `WHERE` clause so the user gets actionable guidance rather than a
/// generic failure.
///
/// `"DELETE"` is passed as the statement label purely so the rendered message
/// reads naturally; the guard's decision does not depend on the statement text.
#[test]
fn rejects_applied_full_table_mutation_without_force_all() {
    // `unwrap_err` asserts the guard returned `Err`; the call would panic here
    // if the guard had (incorrectly) allowed this unsafe combination.
    let error = guard_applied_full_table_mutation("DELETE", true, false, false).unwrap_err();
    assert!(error.to_string().contains("requires a WHERE clause"));
}

/// Verifies the three argument combinations the guard must permit.
///
/// Each `unwrap` asserts the guard returned `Ok`, panicking the test if any of
/// these legitimate cases were wrongly rejected:
/// - dry run (`apply = false`): nothing is mutated, so scope is irrelevant;
/// - applied with `--force-all` (`force_all = true`): the user has explicitly
///   accepted affecting every row;
/// - applied with a filter (`has_filter = true`): a `WHERE` clause already
///   bounds the blast radius.
#[test]
fn allows_dry_run_or_forced_full_table_mutation() {
    // Dry run: not applied, so a full-table scope is harmless.
    guard_applied_full_table_mutation("DELETE", false, false, false).unwrap();
    // Applied but explicitly forced: user has opted into the full-table effect.
    guard_applied_full_table_mutation("DELETE", true, true, false).unwrap();
    // Applied with a filter present: scope is already narrowed by a WHERE clause.
    guard_applied_full_table_mutation("DELETE", true, false, true).unwrap();
}

/// Verifies that parsing a Notion schema flags property types the tool cannot
/// represent, so a `SELECT *` over them can be caught instead of silently
/// dropping columns.
///
/// The fixture defines two properties: a `title` (a fully supported type) and a
/// `formula` (a type this tool does not support, since formula values are
/// computed by Notion and have no faithful local representation). The test
/// asserts that `unsupported_columns` reports the `Formula` property by name.
///
/// `join(" ")` flattens the returned `Vec<String>` into a single string purely
/// to make the `contains` check independent of the vector's length or ordering;
/// the local variable is named `error` because the CLI surfaces this list as a
/// user-facing error/warning, even though the value itself is just joined names.
#[test]
fn rejects_select_all_with_unsupported_columns() {
    // Build a schema from a minimal hand-written stand-in for a Notion database
    // response, mixing one supported and one unsupported property type.
    let schema = DatabaseSchema::from_notion_database(&json!({
        "properties": {
            "Name": { "type": "title", "title": {} },
            "Formula": { "type": "formula", "formula": {} }
        }
    }))
    .unwrap();

    let error = schema.unsupported_columns().join(" ");
    assert!(error.contains("Formula"));
}
