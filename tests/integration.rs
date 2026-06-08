//! Integration tests for the notion-sql crate.
//!
//! These tests require network access to the Notion API and are marked
//! with the `#[ignore]` attribute by default. Run them with:
//!
//! ```bash
//! cargo test --ignored
//! ```

use notion_sql::schema::DatabaseSchema;
use notion_sql::sql::parse_statement;
use serde_json::json;

#[test]
fn database_schema_round_trip() {
    let json_db = json!({
        "properties": {
            "Name": { "type": "title", "title": {} },
            "Status": { "type": "status", "status": {} }
        }
    });

    let schema = DatabaseSchema::from_notion_database(&json_db).unwrap();
    let columns: Vec<String> = schema.available_columns();

    assert!(columns.contains(&"Name".to_string()));
    assert!(columns.contains(&"Status".to_string()));
}

#[test]
fn parse_select_statement() {
    let sql = "SELECT Name, Status FROM database WHERE Status = 'Active'";
    let parsed = parse_statement(sql);

    assert!(parsed.is_ok(), "Failed to parse SELECT statement");
}

#[test]
fn parse_insert_statement() {
    let sql = "INSERT INTO database (Name, Status) VALUES ('Test', 'Active')";
    let parsed = parse_statement(sql);

    assert!(parsed.is_ok(), "Failed to parse INSERT statement");
}

#[test]
fn parse_update_statement() {
    let sql = "UPDATE database SET Status = 'Done' WHERE Name = 'Task 1'";
    let parsed = parse_statement(sql);

    assert!(parsed.is_ok(), "Failed to parse UPDATE statement");
}

#[test]
fn parse_delete_statement() {
    let sql = "DELETE FROM database WHERE Status = 'Archived'";
    let parsed = parse_statement(sql);

    assert!(parsed.is_ok(), "Failed to parse DELETE statement");
}
