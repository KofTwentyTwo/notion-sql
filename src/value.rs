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
