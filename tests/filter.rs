//! Tests for SQL expression to Notion filter translation.

use serde_json::json;
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;

use notion_sql::{filter::translate_where, schema::DatabaseSchema};

fn schema() -> DatabaseSchema {
    DatabaseSchema::from_notion_database(&json!({
        "properties": {
            "Name": { "type": "title", "title": {} },
            "Status": { "type": "status", "status": {} },
            "Tags": { "type": "multi_select", "multi_select": {} },
            "Priority": { "type": "number", "number": {} },
            "Done": { "type": "checkbox", "checkbox": {} },
            "Due": { "type": "date", "date": {} }
        }
    }))
    .unwrap()
}

fn where_expr(sql: &str) -> sqlparser::ast::Expr {
    let parsed = Parser::parse_sql(
        &GenericDialect {},
        &format!("SELECT * FROM Tasks WHERE {sql}"),
    )
    .unwrap();
    let statement = parsed.into_iter().next().unwrap();
    if let sqlparser::ast::Statement::Query(query) = statement {
        if let sqlparser::ast::SetExpr::Select(select) = query.body.as_ref() {
            return select.selection.clone().unwrap();
        }
    }
    panic!("failed to extract WHERE expression")
}

#[test]
fn translates_and_or_grouping() {
    let filter = translate_where(
        &where_expr("Status = 'Done' AND (Priority >= 2 OR Done = true)"),
        &schema(),
    )
    .unwrap();

    assert_eq!(
        filter,
        json!({
            "and": [
                { "property": "Status", "status": { "equals": "Done" } },
                {
                    "or": [
                        { "property": "Priority", "number": { "greater_than_or_equal_to": 2.0 } },
                        { "property": "Done", "checkbox": { "equals": true } }
                    ]
                }
            ]
        })
    );
}

#[test]
fn translates_supported_like_patterns() {
    let cases = [
        (
            "Name LIKE 'task'",
            json!({ "property": "Name", "title": { "equals": "task" } }),
        ),
        (
            "Name LIKE 'task%'",
            json!({ "property": "Name", "title": { "starts_with": "task" } }),
        ),
        (
            "Name ILIKE '%task'",
            json!({ "property": "Name", "title": { "ends_with": "task" } }),
        ),
        (
            "Name LIKE '%task%'",
            json!({ "property": "Name", "title": { "contains": "task" } }),
        ),
        (
            "Name NOT LIKE 'task'",
            json!({ "property": "Name", "title": { "does_not_equal": "task" } }),
        ),
        (
            "Name NOT LIKE '%task%'",
            json!({ "property": "Name", "title": { "does_not_contain": "task" } }),
        ),
    ];

    for (sql, expected) in cases {
        assert_eq!(
            translate_where(&where_expr(sql), &schema()).unwrap(),
            expected
        );
    }
}

#[test]
fn rejects_unsupported_like_patterns() {
    let cases = [
        ("Name LIKE 'ta%sk'", "leading and trailing '%'"),
        ("Name LIKE 'ta_sk'", "LIKE '_' wildcards"),
        ("Name LIKE 'ta\\%sk'", "Escaped LIKE wildcard patterns"),
        ("Name LIKE 'ta#%sk' ESCAPE '#'", "LIKE ESCAPE clauses"),
        ("Name LIKE '%'", "wildcard-only patterns"),
        ("Name NOT LIKE 'task%'", "NOT LIKE prefix patterns"),
        ("Name NOT LIKE '%task'", "NOT LIKE suffix patterns"),
    ];

    for (sql, expected_error) in cases {
        let error = translate_where(&where_expr(sql), &schema())
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(expected_error),
            "expected '{error}' to contain '{expected_error}'"
        );
    }
}

#[test]
fn translates_in_list() {
    let in_list = translate_where(&where_expr("Tags IN ('A', 'B')"), &schema()).unwrap();

    assert_eq!(
        in_list,
        json!({
            "or": [
                { "property": "Tags", "multi_select": { "contains": "A" } },
                { "property": "Tags", "multi_select": { "contains": "B" } }
            ]
        })
    );
}

#[test]
fn translates_null_checks_and_dates() {
    let null_filter = translate_where(&where_expr("Due IS NOT NULL"), &schema()).unwrap();
    let date_filter = translate_where(&where_expr("Due < '2026-01-01'"), &schema()).unwrap();

    assert_eq!(
        null_filter,
        json!({ "property": "Due", "date": { "is_not_empty": true } })
    );
    assert_eq!(
        date_filter,
        json!({ "property": "Due", "date": { "before": "2026-01-01" } })
    );
}

#[test]
fn matches_column_names_case_insensitively() {
    let filter = translate_where(&where_expr("status = 'Done'"), &schema()).unwrap();
    assert_eq!(
        filter,
        json!({ "property": "Status", "status": { "equals": "Done" } })
    );
}
