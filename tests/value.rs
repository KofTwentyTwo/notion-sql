//! Integration tests for [`coerce_property_value`], the boundary that turns a
//! SQL literal (as parsed by `sqlparser`) into the JSON shape the Notion API
//! expects for a single property write.
//!
//! # What is under test
//! Notion is strongly typed: a `Number` property expects `{ "number": n }`, a
//! `Title` expects an array of rich-text spans, a `Status` expects
//! `{ "status": { "name": ... } }`, and so on. SQL, by contrast, hands us loosely
//! typed literals. `coerce_property_value` bridges that gap, and these tests pin
//! down the contract for each [`PropertyType`] variant:
//! - the happy-path payload shape per type (title, number, checkbox, select,
//!   multi-select, ...),
//! - rejection of values that cannot be coerced to the target type,
//! - the special handling of SQL `NULL`, which "clears" a property for the
//!   nullable/collection types but is an error for types that have no empty
//!   representation (e.g. `Title`, `Checkbox`).
//!
//! # Testing approach
//! Rather than construct `sqlparser` AST nodes by hand, each test feeds a SQL
//! snippet through the real parser via [`expr`] and extracts the right-hand-side
//! literal of a synthetic `WHERE` clause. This keeps the tests honest: they
//! exercise the same AST shapes the production query path produces. Expected
//! payloads are written with [`serde_json::json`] and compared structurally, so
//! key ordering in the source literal does not affect the assertion.
//!
//! As a test crate this file defines no public surface; everything here is a
//! helper or a `#[test]` and is private to the integration-test binary.

use notion_sql::{
    schema::{PropertySchema, PropertyType},
    value::coerce_property_value,
};
use serde_json::json;

/// Builds a minimal [`PropertySchema`] for a single column under test.
///
/// The production code keys coercion behavior off the property's
/// [`PropertyType`]; the name is incidental (it appears in error messages), so
/// tests supply a human-readable name alongside the type they want to exercise.
///
/// # Parameters
/// - `name`: the property/column name, copied into the schema as an owned `String`.
/// - `property_type`: the Notion type that drives coercion and payload shape.
///
/// # Returns
/// A `PropertySchema` carrying exactly the supplied name and type.
fn property(name: &str, property_type: PropertyType) -> PropertySchema {
    PropertySchema {
        name: name.to_string(),
        property_type,
    }
}

/// Parses a SQL literal snippet and returns it as a standalone `Expr`.
///
/// There is no public way to ask `sqlparser` for "just a literal expression",
/// so we wrap the snippet in a throwaway `SELECT * FROM t WHERE c = <sql>` and
/// pluck the right-hand side of the resulting `BinaryOp` (`c = <sql>`). That
/// right-hand side is exactly the AST node the production coercion path receives,
/// which is why the tests go through the real parser instead of hand-building
/// `Expr` values.
///
/// # Parameters
/// - `sql`: a SQL literal fragment such as `'Task'`, `3.5`, `true`, or `NULL`.
///   It is interpolated verbatim into the comparison's right-hand side.
///
/// # Returns
/// A cloned, owned copy of the parsed literal expression.
///
/// # Panics
/// Panics if the snippet fails to parse, if the parsed statement is not the
/// expected `SELECT` → `Select` → `WHERE BinaryOp` shape, or if no statement was
/// produced. In a test helper a panic is the desired failure mode: a malformed
/// input should fail loudly rather than silently feed a wrong AST to the coercer.
fn expr(sql: &str) -> sqlparser::ast::Expr {
    // Embed the literal in a complete, parseable statement; `GenericDialect`
    // accepts the broadest syntax so any reasonable literal form is honored.
    let parsed = sqlparser::parser::Parser::parse_sql(
        &sqlparser::dialect::GenericDialect {},
        &format!("SELECT * FROM t WHERE c = {sql}"),
    )
    .unwrap();
    let statement = parsed.into_iter().next().unwrap();
    // Walk the AST down to the WHERE comparison. Each `if let` narrows one level;
    // the literal we want is the `right` operand of the `c = <sql>` binary op.
    if let sqlparser::ast::Statement::Query(query) = statement {
        if let sqlparser::ast::SetExpr::Select(select) = query.body.as_ref() {
            if let Some(sqlparser::ast::Expr::BinaryOp { right, .. }) = &select.selection {
                return right.as_ref().clone();
            }
        }
    }
    // Reached only if the AST did not match the expected shape above.
    panic!("failed to extract literal expression")
}

/// A `Title` literal becomes Notion's rich-text title array.
///
/// Notion models titles as an array of typed rich-text spans, even for a plain
/// string, so a bare SQL string must expand to a single `text` span wrapping the
/// content rather than a scalar.
#[test]
fn coerces_title_to_text_payload() {
    let payload =
        coerce_property_value(&property("Name", PropertyType::Title), &expr("'Task'")).unwrap();
    assert_eq!(
        payload,
        json!({ "title": [{ "type": "text", "text": { "content": "Task" } }] })
    );
}

/// A numeric literal maps to `{ "number": n }`, preserving fractional values.
///
/// Using `3.5` guards against an integer-only coercion path: the value must
/// round-trip as a JSON float, not be truncated.
#[test]
fn coerces_number_to_number_payload() {
    let payload =
        coerce_property_value(&property("Priority", PropertyType::Number), &expr("3.5")).unwrap();
    assert_eq!(payload, json!({ "number": 3.5 }));
}

/// A SQL boolean literal maps to `{ "checkbox": bool }`.
#[test]
fn coerces_checkbox_to_bool_payload() {
    let payload =
        coerce_property_value(&property("Done", PropertyType::Checkbox), &expr("true")).unwrap();
    assert_eq!(payload, json!({ "checkbox": true }));
}

/// Single- and multi-valued choice types produce their respective option shapes.
///
/// A `Status` (here used as the single-choice case) wraps one option name in an
/// object, while `MultiSelect` splits a comma-delimited string into an array of
/// option objects. The `'A, B'` input also asserts that surrounding whitespace
/// after the comma is trimmed (`"B"`, not `" B"`), so option names match Notion's
/// existing options.
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

/// Coercion fails when a literal cannot be interpreted as the target type.
///
/// Feeding the string `'maybe'` to a `Checkbox` must surface an error mentioning
/// the value is "not a valid boolean", confirming the coercer validates rather
/// than silently coercing arbitrary strings to `false`/`true`.
#[test]
fn rejects_type_mismatches() {
    let error = coerce_property_value(&property("Done", PropertyType::Checkbox), &expr("'maybe'"))
        .unwrap_err();
    assert!(error.to_string().contains("not a valid boolean"));
}

/// SQL `NULL` clears each property type that has an "empty" representation.
///
/// Notion clears a property by writing its type-appropriate empty value, which
/// differs per type: rich-text and multi-select clear to an empty array, while
/// number/select/date clear to JSON `null`. This test pins each of those mappings
/// so a future refactor cannot accidentally send `null` where an empty array is
/// required (or vice versa), which Notion would reject or misinterpret.
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

/// SQL `NULL` is rejected for types that have no valid empty representation.
///
/// A `Title` cannot be blank in Notion and a `Checkbox` is always true/false, so
/// neither can be "cleared". Both must return an error whose message contains
/// "cannot be cleared", distinguishing this deliberate rejection from a generic
/// type-mismatch error.
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
