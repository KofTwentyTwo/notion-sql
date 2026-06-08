//! Tests for SQL literal coercion into Notion write payloads.

use notion_sql::{
    schema::{PropertySchema, PropertyType},
    value::coerce_property_value,
};
use serde_json::json;

fn property(name: &str, property_type: PropertyType) -> PropertySchema {
    PropertySchema {
        name: name.to_string(),
        property_type,
    }
}

fn expr(sql: &str) -> sqlparser::ast::Expr {
    let parsed = sqlparser::parser::Parser::parse_sql(
        &sqlparser::dialect::GenericDialect {},
        &format!("SELECT * FROM t WHERE c = {sql}"),
    )
    .unwrap();
    let statement = parsed.into_iter().next().unwrap();
    if let sqlparser::ast::Statement::Query(query) = statement {
        if let sqlparser::ast::SetExpr::Select(select) = query.body.as_ref() {
            if let Some(sqlparser::ast::Expr::BinaryOp { right, .. }) = &select.selection {
                return right.as_ref().clone();
            }
        }
    }
    panic!("failed to extract literal expression")
}

#[test]
fn coerces_title_to_text_payload() {
    let payload =
        coerce_property_value(&property("Name", PropertyType::Title), &expr("'Task'")).unwrap();
    assert_eq!(
        payload,
        json!({ "title": [{ "type": "text", "text": { "content": "Task" } }] })
    );
}

#[test]
fn coerces_number_to_number_payload() {
    let payload =
        coerce_property_value(&property("Priority", PropertyType::Number), &expr("3.5")).unwrap();
    assert_eq!(payload, json!({ "number": 3.5 }));
}

#[test]
fn coerces_checkbox_to_bool_payload() {
    let payload =
        coerce_property_value(&property("Done", PropertyType::Checkbox), &expr("true")).unwrap();
    assert_eq!(payload, json!({ "checkbox": true }));
}

#[test]
fn coerces_select_and_multi_select_payloads() {
    let select =
        coerce_property_value(&property("Status", PropertyType::Status), &expr("'Done'")).unwrap();
    let multi = coerce_property_value(
        &property("Tags", PropertyType::MultiSelect),
        &expr("'A, B'"),
    )
    .unwrap();

    assert_eq!(select, json!({ "status": { "name": "Done" } }));
    assert_eq!(
        multi,
        json!({ "multi_select": [{ "name": "A" }, { "name": "B" }] })
    );
}

#[test]
fn rejects_type_mismatches() {
    let error = coerce_property_value(&property("Done", PropertyType::Checkbox), &expr("'maybe'"))
        .unwrap_err();
    assert!(error.to_string().contains("not a valid boolean"));
}

#[test]
fn clears_nullable_properties_with_null() {
    assert_eq!(
        coerce_property_value(&property("Notes", PropertyType::RichText), &expr("NULL")).unwrap(),
        json!({ "rich_text": [] })
    );
    assert_eq!(
        coerce_property_value(&property("Priority", PropertyType::Number), &expr("NULL")).unwrap(),
        json!({ "number": null })
    );
    assert_eq!(
        coerce_property_value(&property("Status", PropertyType::Select), &expr("NULL")).unwrap(),
        json!({ "select": null })
    );
    assert_eq!(
        coerce_property_value(&property("Tags", PropertyType::MultiSelect), &expr("NULL")).unwrap(),
        json!({ "multi_select": [] })
    );
    assert_eq!(
        coerce_property_value(&property("Due", PropertyType::Date), &expr("NULL")).unwrap(),
        json!({ "date": null })
    );
}

#[test]
fn rejects_null_for_non_clearable_properties() {
    let title_error =
        coerce_property_value(&property("Name", PropertyType::Title), &expr("NULL")).unwrap_err();
    let checkbox_error =
        coerce_property_value(&property("Done", PropertyType::Checkbox), &expr("NULL"))
            .unwrap_err();

    assert!(title_error.to_string().contains("cannot be cleared"));
    assert!(checkbox_error.to_string().contains("cannot be cleared"));
}
