//! Tests for CLI-level safety checks that do not require Notion HTTP access.

use notion_sql::{cli::guard_applied_full_table_mutation, schema::DatabaseSchema};
use serde_json::json;

#[test]
fn rejects_applied_full_table_mutation_without_force_all() {
    let error = guard_applied_full_table_mutation("DELETE", true, false, false).unwrap_err();
    assert!(error.to_string().contains("requires a WHERE clause"));
}

#[test]
fn allows_dry_run_or_forced_full_table_mutation() {
    guard_applied_full_table_mutation("DELETE", false, false, false).unwrap();
    guard_applied_full_table_mutation("DELETE", true, true, false).unwrap();
    guard_applied_full_table_mutation("DELETE", true, false, true).unwrap();
}

#[test]
fn rejects_select_all_with_unsupported_columns() {
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
