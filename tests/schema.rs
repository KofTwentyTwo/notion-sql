//! Tests for Notion schema parsing and property lookup safety.

use notion_sql::schema::DatabaseSchema;
use serde_json::json;

#[test]
fn rejects_case_insensitive_property_name_collisions() {
    let error = DatabaseSchema::from_notion_database(&json!({
        "properties": {
            "Status": { "type": "status", "status": {} },
            "status": { "type": "select", "select": {} }
        }
    }))
    .unwrap_err()
    .to_string();

    assert!(error.contains("Ambiguous Notion property names"));
    assert!(error.contains("Status"));
    assert!(error.contains("status"));
}

#[test]
fn lists_unsupported_columns() {
    let schema = DatabaseSchema::from_notion_database(&json!({
        "properties": {
            "Name": { "type": "title", "title": {} },
            "Formula": { "type": "formula", "formula": {} }
        }
    }))
    .unwrap();

    assert_eq!(
        schema.unsupported_columns(),
        vec!["Formula (formula)".to_string()]
    );
}
