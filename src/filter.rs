//! SQL `WHERE` expression translation for Notion database filters.
//!
//! `sqlparser` keeps SQL expressions in a database-neutral AST. This module
//! narrows that AST to the comparison forms that the Notion filter API can
//! represent for the supported property types.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value as JsonValue};
use sqlparser::ast::{BinaryOperator, Expr};

use crate::schema::{DatabaseSchema, PropertySchema, PropertyType};
use crate::value::{literal_from_expr, Literal};

/// Internal comparison model shared by SQL operators and Notion filter conditions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ComparisonOp {
    /// Equality comparison.
    Eq,
    /// Inequality comparison.
    NotEq,
    /// Greater-than comparison.
    Gt,
    /// Less-than comparison.
    Lt,
    /// Greater-than-or-equal comparison.
    GtEq,
    /// Less-than-or-equal comparison.
    LtEq,
    /// Text containment comparison used for `LIKE` and selected property types.
    Contains,
    /// Negative text containment comparison used for `NOT LIKE`.
    DoesNotContain,
    /// Prefix comparison used for `LIKE 'value%'`.
    StartsWith,
    /// Suffix comparison used for `LIKE '%value'`.
    EndsWith,
}

/// Translates a supported SQL `WHERE` expression into a Notion filter object.
pub fn translate_where(expr: &Expr, schema: &DatabaseSchema) -> Result<JsonValue> {
    match expr {
        Expr::BinaryOp { left, op, right } if *op == BinaryOperator::And => Ok(json!({
            "and": [
                translate_where(left, schema)?,
                translate_where(right, schema)?
            ]
        })),
        Expr::BinaryOp { left, op, right } if *op == BinaryOperator::Or => Ok(json!({
            "or": [
                translate_where(left, schema)?,
                translate_where(right, schema)?
            ]
        })),
        Expr::BinaryOp { left, op, right } => {
            let comparison = comparison_from_binary_operator(op)?;
            let property = resolve_column(left, schema)?;
            let literal = literal_from_expr(right)
                .with_context(|| format!("Invalid comparison value for '{}'", property.name))?;
            property_filter(property, comparison, &literal)
        }
        Expr::Like {
            negated,
            any,
            expr,
            pattern,
            escape_char,
        }
        | Expr::ILike {
            negated,
            any,
            expr,
            pattern,
            escape_char,
        } => {
            if *any {
                bail!("LIKE ANY is not supported");
            }
            if escape_char.is_some() {
                bail!("LIKE ESCAPE clauses are not supported");
            }
            let property = resolve_column(expr, schema)?;
            let literal = literal_from_expr(pattern)
                .with_context(|| format!("Invalid LIKE pattern for '{}'", property.name))?;
            let (comparison, value) = like_pattern_to_comparison(&literal.as_string()?, *negated)?;
            let literal = Literal::String(value);
            property_filter(property, comparison, &literal)
        }
        Expr::InList {
            expr,
            list,
            negated,
        } => {
            if *negated {
                bail!("NOT IN is not supported");
            }
            let property = resolve_column(expr, schema)?;
            let filters = list
                .iter()
                .map(|item| {
                    // Notion has no direct SQL `IN` operator, so each item becomes
                    // an equality filter and the list is joined with OR.
                    let literal = literal_from_expr(item)
                        .with_context(|| format!("Invalid IN value for '{}'", property.name))?;
                    property_filter(property, ComparisonOp::Eq, &literal)
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(json!({ "or": filters }))
        }
        Expr::IsNull(expr) => {
            let property = resolve_column(expr, schema)?;
            Ok(empty_filter(property, true))
        }
        Expr::IsNotNull(expr) => {
            let property = resolve_column(expr, schema)?;
            Ok(empty_filter(property, false))
        }
        Expr::Nested(expr) => translate_where(expr, schema),
        other => bail!("Unsupported WHERE expression '{other}'"),
    }
}

/// Resolves an expression that must be a column reference into a database property.
fn resolve_column<'a>(expr: &Expr, schema: &'a DatabaseSchema) -> Result<&'a PropertySchema> {
    let column = column_name(expr)?;
    schema.resolve_property(&column)
}

/// Extracts an unqualified column name from a SQL expression.
fn column_name(expr: &Expr) -> Result<String> {
    match expr {
        Expr::Identifier(ident) => Ok(ident.value.clone()),
        Expr::CompoundIdentifier(parts) if parts.len() == 1 => Ok(parts[0].value.clone()),
        Expr::CompoundIdentifier(_) => bail!("Qualified column names are not supported"),
        other => bail!("Expected a column name, got '{other}'"),
    }
}

/// Converts supported SQL binary operators into the internal comparison enum.
fn comparison_from_binary_operator(op: &BinaryOperator) -> Result<ComparisonOp> {
    match op {
        BinaryOperator::Eq => Ok(ComparisonOp::Eq),
        BinaryOperator::NotEq => Ok(ComparisonOp::NotEq),
        BinaryOperator::Gt => Ok(ComparisonOp::Gt),
        BinaryOperator::Lt => Ok(ComparisonOp::Lt),
        BinaryOperator::GtEq => Ok(ComparisonOp::GtEq),
        BinaryOperator::LtEq => Ok(ComparisonOp::LtEq),
        other => bail!("Unsupported comparison operator '{other}'"),
    }
}

/// Builds the property-specific Notion filter payload for a single comparison.
fn property_filter(
    property: &PropertySchema,
    op: ComparisonOp,
    literal: &Literal,
) -> Result<JsonValue> {
    let key = property.property_type.notion_key();
    let condition = match property.property_type {
        PropertyType::Title | PropertyType::RichText => text_condition(op, literal)?,
        PropertyType::Select | PropertyType::Status => select_condition(op, literal)?,
        PropertyType::MultiSelect => multi_select_condition(op, literal)?,
        PropertyType::Number => number_condition(op, literal)?,
        PropertyType::Checkbox => checkbox_condition(op, literal)?,
        PropertyType::Date => date_condition(op, literal)?,
        PropertyType::Unsupported(ref value) => {
            bail!(
                "Column '{}' has unsupported Notion property type '{}'",
                property.name,
                value
            )
        }
    };

    Ok(json!({
        "property": property.name,
        key: condition
    }))
}

/// Builds a Notion text condition for title and rich text properties.
fn text_condition(op: ComparisonOp, literal: &Literal) -> Result<JsonValue> {
    let value = literal.as_string()?;
    match op {
        ComparisonOp::Eq => Ok(json!({ "equals": value })),
        ComparisonOp::NotEq => Ok(json!({ "does_not_equal": value })),
        ComparisonOp::Contains => Ok(json!({ "contains": value })),
        ComparisonOp::DoesNotContain => Ok(json!({ "does_not_contain": value })),
        ComparisonOp::StartsWith => Ok(json!({ "starts_with": value })),
        ComparisonOp::EndsWith => Ok(json!({ "ends_with": value })),
        other => bail!("Text properties do not support {other:?} comparisons"),
    }
}

/// Builds a Notion select or status condition.
fn select_condition(op: ComparisonOp, literal: &Literal) -> Result<JsonValue> {
    let value = literal.as_string()?;
    match op {
        ComparisonOp::Eq => Ok(json!({ "equals": value })),
        ComparisonOp::NotEq => Ok(json!({ "does_not_equal": value })),
        other => bail!("Select/status properties do not support {other:?} comparisons"),
    }
}

/// Builds a Notion multi-select condition.
fn multi_select_condition(op: ComparisonOp, literal: &Literal) -> Result<JsonValue> {
    let value = literal.as_string()?;
    match op {
        ComparisonOp::Eq | ComparisonOp::Contains => Ok(json!({ "contains": value })),
        ComparisonOp::NotEq | ComparisonOp::DoesNotContain => {
            Ok(json!({ "does_not_contain": value }))
        }
        other => bail!("Multi-select properties do not support {other:?} comparisons"),
    }
}

/// Builds a Notion number condition.
fn number_condition(op: ComparisonOp, literal: &Literal) -> Result<JsonValue> {
    let value = literal.as_number()?;
    match op {
        ComparisonOp::Eq => Ok(json!({ "equals": value })),
        ComparisonOp::NotEq => Ok(json!({ "does_not_equal": value })),
        ComparisonOp::Gt => Ok(json!({ "greater_than": value })),
        ComparisonOp::Lt => Ok(json!({ "less_than": value })),
        ComparisonOp::GtEq => Ok(json!({ "greater_than_or_equal_to": value })),
        ComparisonOp::LtEq => Ok(json!({ "less_than_or_equal_to": value })),
        other => bail!("Number properties do not support {other:?} comparisons"),
    }
}

/// Builds a Notion checkbox condition.
fn checkbox_condition(op: ComparisonOp, literal: &Literal) -> Result<JsonValue> {
    let value = literal.as_bool()?;
    match op {
        ComparisonOp::Eq => Ok(json!({ "equals": value })),
        ComparisonOp::NotEq => Ok(json!({ "does_not_equal": value })),
        other => bail!("Checkbox properties do not support {other:?} comparisons"),
    }
}

/// Builds a Notion date condition.
fn date_condition(op: ComparisonOp, literal: &Literal) -> Result<JsonValue> {
    let value = literal.as_string()?;
    match op {
        ComparisonOp::Eq => Ok(json!({ "equals": value })),
        ComparisonOp::NotEq => Ok(json!({ "does_not_equal": value })),
        ComparisonOp::Gt => Ok(json!({ "after": value })),
        ComparisonOp::Lt => Ok(json!({ "before": value })),
        ComparisonOp::GtEq => Ok(json!({ "on_or_after": value })),
        ComparisonOp::LtEq => Ok(json!({ "on_or_before": value })),
        other => bail!("Date properties do not support {other:?} comparisons"),
    }
}

/// Builds an `IS NULL` or `IS NOT NULL` Notion filter for a property.
fn empty_filter(property: &PropertySchema, is_empty: bool) -> JsonValue {
    let condition = if is_empty {
        json!({ "is_empty": true })
    } else {
        json!({ "is_not_empty": true })
    };

    json!({
        "property": property.name,
        property.property_type.notion_key(): condition
    })
}

/// Converts a supported SQL `LIKE` pattern into a Notion comparison and value.
fn like_pattern_to_comparison(pattern: &str, negated: bool) -> Result<(ComparisonOp, String)> {
    if pattern.contains('\\') {
        bail!("Escaped LIKE wildcard patterns are not supported");
    }
    if pattern.contains('_') {
        bail!("LIKE '_' wildcards are not supported");
    }

    let percent_count = pattern.chars().filter(|value| *value == '%').count();
    if percent_count > 0 && pattern.chars().all(|value| value == '%') {
        bail!("LIKE wildcard-only patterns are not supported");
    }

    let starts_with_percent = pattern.starts_with('%');
    let ends_with_percent = pattern.ends_with('%');

    let (comparison, value) = match (starts_with_percent, ends_with_percent, percent_count) {
        (_, _, 0) => (ComparisonOp::Eq, pattern.to_string()),
        (false, true, 1) => (
            ComparisonOp::StartsWith,
            pattern.trim_end_matches('%').to_string(),
        ),
        (true, false, 1) => (
            ComparisonOp::EndsWith,
            pattern.trim_start_matches('%').to_string(),
        ),
        (true, true, 2) => (
            ComparisonOp::Contains,
            pattern
                .trim_start_matches('%')
                .trim_end_matches('%')
                .to_string(),
        ),
        _ => bail!("LIKE patterns only support leading and trailing '%' wildcards"),
    };

    match (comparison, negated) {
        (ComparisonOp::Eq, false) => Ok((ComparisonOp::Eq, value)),
        (ComparisonOp::Eq, true) => Ok((ComparisonOp::NotEq, value)),
        (ComparisonOp::Contains, false) => Ok((ComparisonOp::Contains, value)),
        (ComparisonOp::Contains, true) => Ok((ComparisonOp::DoesNotContain, value)),
        (ComparisonOp::StartsWith, false) => Ok((ComparisonOp::StartsWith, value)),
        (ComparisonOp::StartsWith, true) => {
            bail!("NOT LIKE prefix patterns are not supported by Notion")
        }
        (ComparisonOp::EndsWith, false) => Ok((ComparisonOp::EndsWith, value)),
        (ComparisonOp::EndsWith, true) => {
            bail!("NOT LIKE suffix patterns are not supported by Notion")
        }
        _ => unreachable!("LIKE pattern analysis only emits LIKE comparison operators"),
    }
}

#[cfg(test)]
mod tests {
    //! Tests for SQL expression to Notion filter translation.

    use serde_json::json;
    use sqlparser::dialect::GenericDialect;
    use sqlparser::parser::Parser;

    use super::*;
    use crate::schema::DatabaseSchema;

    /// Builds a schema with one property for each supported filter family.
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

    /// Parses a SQL fragment as the right side of a `WHERE` clause and returns its expression.
    fn where_expr(sql: &str) -> Expr {
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

    /// Verifies that nested AND and OR expressions preserve their grouping.
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

    /// Verifies that `LIKE` pattern shapes map to the matching Notion operators.
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

    /// Verifies that unsupported `LIKE` wildcard forms fail clearly.
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

    /// Verifies that `IN` maps to a Notion OR filter.
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

    /// Verifies Notion empty checks and date comparison operator mapping.
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

    /// Verifies that schema lookup accepts SQL column names case-insensitively.
    #[test]
    fn matches_column_names_case_insensitively() {
        let filter = translate_where(&where_expr("status = 'Done'"), &schema()).unwrap();
        assert_eq!(
            filter,
            json!({ "property": "Status", "status": { "equals": "Done" } })
        );
    }
}
