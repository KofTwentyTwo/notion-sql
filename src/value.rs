//! SQL literal extraction and Notion property value coercion.
//!
//! Notion writes use property-type-specific JSON shapes. This module converts
//! SQL literal expressions into those shapes after schema resolution.

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value as JsonValue};
use sqlparser::ast::{Expr, UnaryOperator, Value};

use crate::schema::{PropertySchema, PropertyType};

/// Literal values accepted from SQL expressions.
#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    /// String literal.
    String(String),
    /// Numeric literal stored as `f64`, matching JSON number handling.
    Number(f64),
    /// Boolean literal.
    Bool(bool),
    /// SQL `NULL` literal.
    Null,
}

impl Literal {
    /// Converts the literal into the text value accepted by Notion text-like properties.
    pub fn as_string(&self) -> Result<String> {
        match self {
            Self::String(value) => Ok(value.clone()),
            Self::Number(value) => Ok(number_to_string(*value)),
            Self::Bool(value) => Ok(value.to_string()),
            Self::Null => bail!("NULL cannot be used as a text value"),
        }
    }

    /// Converts the literal into a number, accepting numeric strings for CLI convenience.
    pub fn as_number(&self) -> Result<f64> {
        match self {
            Self::Number(value) => Ok(*value),
            Self::String(value) => value
                .parse::<f64>()
                .with_context(|| format!("'{value}' is not a valid number")),
            Self::Bool(_) => bail!("Boolean values cannot be used as numbers"),
            Self::Null => bail!("NULL cannot be used as a number"),
        }
    }

    /// Converts the literal into a boolean, accepting `true` and `false` strings.
    pub fn as_bool(&self) -> Result<bool> {
        match self {
            Self::Bool(value) => Ok(*value),
            Self::String(value) if value.eq_ignore_ascii_case("true") => Ok(true),
            Self::String(value) if value.eq_ignore_ascii_case("false") => Ok(false),
            Self::String(value) => bail!("'{value}' is not a valid boolean"),
            Self::Number(_) => bail!("Numbers cannot be used as booleans"),
            Self::Null => bail!("NULL cannot be used as a boolean"),
        }
    }
}

/// Extracts a supported literal from a SQL expression.
pub fn literal_from_expr(expr: &Expr) -> Result<Literal> {
    match expr {
        Expr::Value(value) => literal_from_value(&value.value),
        Expr::UnaryOp {
            op: UnaryOperator::Minus,
            expr,
        } => match literal_from_expr(expr)? {
            Literal::Number(value) => Ok(Literal::Number(-value)),
            other => bail!("Unary minus requires a number, got {other:?}"),
        },
        Expr::UnaryOp {
            op: UnaryOperator::Plus,
            expr,
        } => literal_from_expr(expr),
        other => Err(anyhow!("Expected a literal value, got '{other}'")),
    }
}

/// Converts a `sqlparser` literal value into the local literal representation.
fn literal_from_value(value: &Value) -> Result<Literal> {
    match value {
        Value::SingleQuotedString(value)
        | Value::DoubleQuotedString(value)
        | Value::TripleSingleQuotedString(value)
        | Value::TripleDoubleQuotedString(value)
        | Value::EscapedStringLiteral(value)
        | Value::UnicodeStringLiteral(value)
        | Value::NationalStringLiteral(value) => Ok(Literal::String(value.clone())),
        Value::Number(value, _) => value
            .parse::<f64>()
            .map(Literal::Number)
            .with_context(|| format!("'{value}' is not a valid number")),
        Value::Boolean(value) => Ok(Literal::Bool(*value)),
        Value::Null => Ok(Literal::Null),
        other => Err(anyhow!("Unsupported SQL literal '{other}'")),
    }
}

/// Coerces a SQL expression into the Notion JSON value for one property.
pub fn coerce_property_value(property: &PropertySchema, expr: &Expr) -> Result<JsonValue> {
    let literal = literal_from_expr(expr)
        .with_context(|| format!("Invalid value for column '{}'", property.name))?;
    coerce_literal(property, &literal)
}

/// Coerces a local literal into the Notion JSON shape for one property.
pub fn coerce_literal(property: &PropertySchema, literal: &Literal) -> Result<JsonValue> {
    if matches!(literal, Literal::Null) {
        return clear_property_value(property);
    }

    match property.property_type {
        PropertyType::Title => Ok(json!({
            "title": [{
                "type": "text",
                "text": { "content": literal.as_string()? }
            }]
        })),
        PropertyType::RichText => Ok(json!({
            "rich_text": [{
                "type": "text",
                "text": { "content": literal.as_string()? }
            }]
        })),
        PropertyType::Number => Ok(json!({ "number": literal.as_number()? })),
        PropertyType::Checkbox => Ok(json!({ "checkbox": literal.as_bool()? })),
        PropertyType::Select => Ok(json!({ "select": { "name": literal.as_string()? } })),
        PropertyType::Status => Ok(json!({ "status": { "name": literal.as_string()? } })),
        PropertyType::MultiSelect => {
            // The CLI accepts comma-separated multi-select values because SQL
            // scalar literals cannot directly represent Notion option arrays.
            let names = literal
                .as_string()?
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|name| json!({ "name": name }))
                .collect::<Vec<_>>();
            Ok(json!({ "multi_select": names }))
        }
        PropertyType::Date => Ok(json!({ "date": { "start": literal.as_string()? } })),
        PropertyType::Unsupported(ref value) => {
            bail!(
                "Column '{}' has unsupported Notion property type '{}'",
                property.name,
                value
            )
        }
    }
}

/// Builds the Notion payload used to clear one writable property.
fn clear_property_value(property: &PropertySchema) -> Result<JsonValue> {
    match property.property_type {
        PropertyType::Title => bail!("Title properties cannot be cleared with NULL"),
        PropertyType::RichText => Ok(json!({ "rich_text": [] })),
        PropertyType::Number => Ok(json!({ "number": null })),
        PropertyType::Checkbox => bail!("Checkbox properties cannot be cleared with NULL"),
        PropertyType::Select => Ok(json!({ "select": null })),
        PropertyType::Status => bail!("Status properties cannot be cleared with NULL"),
        PropertyType::MultiSelect => Ok(json!({ "multi_select": [] })),
        PropertyType::Date => Ok(json!({ "date": null })),
        PropertyType::Unsupported(ref value) => {
            bail!(
                "Column '{}' has unsupported Notion property type '{}'",
                property.name,
                value
            )
        }
    }
}

/// Formats integral floats without a fractional suffix for text coercion.
fn number_to_string(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    //! Tests for SQL literal coercion into Notion write payloads.

    use sqlparser::dialect::GenericDialect;
    use sqlparser::parser::Parser;

    use super::*;

    /// Builds a property schema for coercion tests.
    fn property(name: &str, property_type: PropertyType) -> PropertySchema {
        PropertySchema {
            name: name.to_string(),
            property_type,
        }
    }

    /// Parses a SQL literal by embedding it in a simple comparison expression.
    fn expr(sql: &str) -> Expr {
        let parsed = Parser::parse_sql(
            &GenericDialect {},
            &format!("SELECT * FROM t WHERE c = {sql}"),
        )
        .unwrap();
        let statement = parsed.into_iter().next().unwrap();
        if let sqlparser::ast::Statement::Query(query) = statement {
            if let sqlparser::ast::SetExpr::Select(select) = query.body.as_ref() {
                if let Some(Expr::BinaryOp { right, .. }) = &select.selection {
                    return right.as_ref().clone();
                }
            }
        }
        panic!("failed to extract literal expression")
    }

    /// Verifies title values become Notion rich text objects.
    #[test]
    fn coerces_title_to_text_payload() {
        let payload =
            coerce_property_value(&property("Name", PropertyType::Title), &expr("'Task'")).unwrap();
        assert_eq!(
            payload,
            json!({ "title": [{ "type": "text", "text": { "content": "Task" } }] })
        );
    }

    /// Verifies numeric values become Notion number payloads.
    #[test]
    fn coerces_number_to_number_payload() {
        let payload =
            coerce_property_value(&property("Priority", PropertyType::Number), &expr("3.5"))
                .unwrap();
        assert_eq!(payload, json!({ "number": 3.5 }));
    }

    /// Verifies boolean values become Notion checkbox payloads.
    #[test]
    fn coerces_checkbox_to_bool_payload() {
        let payload =
            coerce_property_value(&property("Done", PropertyType::Checkbox), &expr("true"))
                .unwrap();
        assert_eq!(payload, json!({ "checkbox": true }));
    }

    /// Verifies status and multi-select option payload generation.
    #[test]
    fn coerces_select_and_multi_select_payloads() {
        let select =
            coerce_property_value(&property("Status", PropertyType::Status), &expr("'Done'"))
                .unwrap();
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

    /// Verifies invalid literals fail before a write payload is produced.
    #[test]
    fn rejects_type_mismatches() {
        let error =
            coerce_property_value(&property("Done", PropertyType::Checkbox), &expr("'maybe'"))
                .unwrap_err();
        assert!(error.to_string().contains("not a valid boolean"));
    }

    /// Verifies writable nullable properties can be cleared using SQL NULL.
    #[test]
    fn clears_nullable_properties_with_null() {
        assert_eq!(
            coerce_property_value(&property("Notes", PropertyType::RichText), &expr("NULL"))
                .unwrap(),
            json!({ "rich_text": [] })
        );
        assert_eq!(
            coerce_property_value(&property("Priority", PropertyType::Number), &expr("NULL"))
                .unwrap(),
            json!({ "number": null })
        );
        assert_eq!(
            coerce_property_value(&property("Status", PropertyType::Select), &expr("NULL"))
                .unwrap(),
            json!({ "select": null })
        );
        assert_eq!(
            coerce_property_value(&property("Tags", PropertyType::MultiSelect), &expr("NULL"))
                .unwrap(),
            json!({ "multi_select": [] })
        );
        assert_eq!(
            coerce_property_value(&property("Due", PropertyType::Date), &expr("NULL")).unwrap(),
            json!({ "date": null })
        );
    }

    /// Verifies required or boolean properties reject ambiguous NULL clearing.
    #[test]
    fn rejects_null_for_non_clearable_properties() {
        let title_error =
            coerce_property_value(&property("Name", PropertyType::Title), &expr("NULL"))
                .unwrap_err();
        let checkbox_error =
            coerce_property_value(&property("Done", PropertyType::Checkbox), &expr("NULL"))
                .unwrap_err();

        assert!(title_error.to_string().contains("cannot be cleared"));
        assert!(checkbox_error.to_string().contains("cannot be cleared"));
    }
}
