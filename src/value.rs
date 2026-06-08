//! SQL literal extraction and Notion property value coercion.
//!
//! # Purpose
//!
//! Notion's write API does not accept plain scalars; every property must be sent
//! as a property-type-specific JSON shape (for example a title is an array of
//! rich-text runs, a number is `{"number": n}`, a select is `{"select":{"name":..}}`).
//! This module is the bridge between the SQL side of the crate and that wire
//! format: it turns the literal expressions produced by the SQL parser into the
//! JSON shape required for whichever Notion property a column maps to.
//!
//! # Responsibilities
//!
//! - Define [`Literal`], a small, parser-independent representation of the scalar
//!   values we accept in `INSERT`/`UPDATE` statements, and the type-coercion
//!   helpers ([`Literal::as_string`], [`Literal::as_number`], [`Literal::as_bool`]).
//! - Extract a [`Literal`] from a `sqlparser` [`Expr`] ([`literal_from_expr`]),
//!   including the unary `+`/`-` sign-prefix cases the parser emits separately.
//! - Coerce a literal (or a raw expression) into the Notion JSON payload for a
//!   specific resolved property ([`coerce_property_value`], [`coerce_literal`]),
//!   and produce the distinct "clear this property" payload for SQL `NULL`
//!   (`clear_property_value`).
//!
//! # Where it fits in the crate
//!
//! Callers first resolve a column to a [`PropertySchema`] (via the `schema`
//! module), then hand both the schema and the SQL value expression here. The
//! returned [`JsonValue`] is merged into the `properties` object of a Notion
//! page create/update request. Schema resolution must happen first because the
//! same SQL literal coerces differently depending on the target property type.

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value as JsonValue};
use sqlparser::ast::{Expr, UnaryOperator, Value};

use crate::schema::{PropertySchema, PropertyType};

/// A scalar value accepted from a SQL expression, decoupled from `sqlparser`.
///
/// This is the crate-internal normal form for SQL literals. The parser exposes
/// many string-literal variants (single/double/triple-quoted, escaped, national,
/// unicode) and stores numbers as undecoded text; collapsing all of that into
/// these four cases keeps the coercion logic in this module simple and gives the
/// rest of the crate a stable type to match on.
///
/// `Number` is held as `f64` deliberately: Notion numbers are JSON numbers, so
/// using `f64` avoids an int/float split and round-trips cleanly through
/// `serde_json`. The trade-off is the usual loss of precision for very large
/// integers, which is acceptable for the values Notion stores.
#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    /// A text value, already unescaped/unquoted by the parser.
    String(String),
    /// A numeric value stored as `f64` to match JSON number semantics.
    Number(f64),
    /// A boolean value (`TRUE`/`FALSE`).
    Bool(bool),
    /// The SQL `NULL` literal; signals "clear this property" downstream.
    Null,
}

impl Literal {
    /// Coerces this literal into the text required by Notion text-like properties
    /// (title, rich text, select/status names, date strings).
    ///
    /// Numbers are rendered via `number_to_string` so that integral values lose
    /// their `.0` suffix, and booleans render as `"true"`/`"false"`. This loose
    /// coercion is intentional: a user may write a number or boolean literal into
    /// a text column and expect a sensible string.
    ///
    /// # Errors
    ///
    /// Returns an error for [`Literal::Null`], since `NULL` is handled separately
    /// as a "clear" operation and must never be stringified into a text value.
    pub fn as_string(&self) -> Result<String> {
        match self {
            Self::String(value) => Ok(value.clone()),
            Self::Number(value) => Ok(number_to_string(*value)),
            Self::Bool(value) => Ok(value.to_string()),
            Self::Null => bail!("NULL cannot be used as a text value"),
        }
    }

    /// Coerces this literal into an `f64` for Notion number properties.
    ///
    /// A `String` variant is parsed as a number for CLI convenience: quoting a
    /// numeric value (`'42'`) is common and should still target a number column.
    ///
    /// # Errors
    ///
    /// Returns an error when a `String` does not parse as a number, and for
    /// `Bool` and `Null`, neither of which has a meaningful numeric value.
    pub fn as_number(&self) -> Result<f64> {
        match self {
            Self::Number(value) => Ok(*value),
            // Accept numeric strings so quoted numbers still write to a number column.
            Self::String(value) => value
                .parse::<f64>()
                .with_context(|| format!("'{value}' is not a valid number")),
            Self::Bool(_) => bail!("Boolean values cannot be used as numbers"),
            Self::Null => bail!("NULL cannot be used as a number"),
        }
    }

    /// Coerces this literal into a `bool` for Notion checkbox properties.
    ///
    /// Case-insensitive `"true"`/`"false"` strings are accepted so a quoted
    /// boolean still targets a checkbox column. Numeric truthiness is deliberately
    /// not supported to avoid the ambiguity of treating `0`/`1` as booleans.
    ///
    /// # Errors
    ///
    /// Returns an error for a `String` that is neither `"true"` nor `"false"`,
    /// and for `Number` and `Null`, which have no boolean interpretation here.
    pub fn as_bool(&self) -> Result<bool> {
        match self {
            Self::Bool(value) => Ok(*value),
            // Tolerate quoted booleans in either case so 'TRUE' and 'true' both work.
            Self::String(value) if value.eq_ignore_ascii_case("true") => Ok(true),
            Self::String(value) if value.eq_ignore_ascii_case("false") => Ok(false),
            Self::String(value) => bail!("'{value}' is not a valid boolean"),
            Self::Number(_) => bail!("Numbers cannot be used as booleans"),
            Self::Null => bail!("NULL cannot be used as a boolean"),
        }
    }
}

/// Extracts a supported [`Literal`] from a parsed SQL expression.
///
/// Only literal-shaped expressions are accepted: a bare value, or a value behind
/// a unary `+`/`-` sign. The sign cases exist because `sqlparser` does not fold
/// signs into numeric literals — it parses `-5` as a `UnaryOp` wrapping `5` — so
/// we recurse to unwrap the operand and apply the sign ourselves. Arbitrary
/// expressions (column references, function calls, arithmetic) are rejected
/// because this crate only writes literal values, not computed ones.
///
/// # Parameters
///
/// - `expr`: the SQL expression to interpret as a single literal value.
///
/// # Errors
///
/// Returns an error if `expr` is not a literal, if unary minus is applied to a
/// non-number, or if the underlying value is an unsupported SQL literal kind
/// (propagated from `literal_from_value`).
pub fn literal_from_expr(expr: &Expr) -> Result<Literal> {
    match expr {
        Expr::Value(value) => literal_from_value(&value.value),
        // `sqlparser` represents a leading minus as a UnaryOp, not a signed
        // number literal, so recurse and negate the resolved operand.
        Expr::UnaryOp {
            op: UnaryOperator::Minus,
            expr,
        } => match literal_from_expr(expr)? {
            Literal::Number(value) => Ok(Literal::Number(-value)),
            other => bail!("Unary minus requires a number, got {other:?}"),
        },
        // A leading plus is a no-op sign; just unwrap and return the operand.
        Expr::UnaryOp {
            op: UnaryOperator::Plus,
            expr,
        } => literal_from_expr(expr),
        other => Err(anyhow!("Expected a literal value, got '{other}'")),
    }
}

/// Converts a `sqlparser` [`Value`] into the crate's [`Literal`] representation.
///
/// All of the parser's string-literal variants collapse to [`Literal::String`]
/// because, once parsed, the surface syntax (quote style, escaping, national /
/// unicode prefixes) no longer matters — only the decoded text does. Numbers are
/// stored by the parser as undecoded text, so we parse them to `f64` here.
///
/// # Parameters
///
/// - `value`: the parsed SQL literal value to normalize.
///
/// # Errors
///
/// Returns an error if a numeric literal fails to parse as `f64`, or if the
/// value is a literal kind this crate does not support (for example placeholders
/// or interval/dollar-quoted forms).
fn literal_from_value(value: &Value) -> Result<Literal> {
    match value {
        // Every string flavor decodes to the same text; quote/escape style is irrelevant here.
        Value::SingleQuotedString(value)
        | Value::DoubleQuotedString(value)
        | Value::TripleSingleQuotedString(value)
        | Value::TripleDoubleQuotedString(value)
        | Value::EscapedStringLiteral(value)
        | Value::UnicodeStringLiteral(value)
        | Value::NationalStringLiteral(value) => Ok(Literal::String(value.clone())),
        // The parser keeps numbers as text (the second field is the "is big decimal"
        // flag, ignored here); parse to f64 to match JSON number handling.
        Value::Number(value, _) => value
            .parse::<f64>()
            .map(Literal::Number)
            .with_context(|| format!("'{value}' is not a valid number")),
        Value::Boolean(value) => Ok(Literal::Bool(*value)),
        Value::Null => Ok(Literal::Null),
        other => Err(anyhow!("Unsupported SQL literal '{other}'")),
    }
}

/// Coerces a raw SQL value expression into the Notion JSON payload for one property.
///
/// Convenience wrapper that extracts the literal from `expr` and then defers to
/// [`coerce_literal`]. This is the entry point callers use when they hold the
/// parsed expression directly (for example a value from an `INSERT`/`SET` clause).
///
/// # Parameters
///
/// - `property`: the resolved schema for the target column, which decides the
///   output JSON shape.
/// - `expr`: the SQL value expression to coerce.
///
/// # Errors
///
/// Returns an error if `expr` is not a usable literal (with the column name added
/// for context) or if the literal cannot be coerced to the property's type
/// (propagated from [`coerce_literal`]).
pub fn coerce_property_value(property: &PropertySchema, expr: &Expr) -> Result<JsonValue> {
    let literal = literal_from_expr(expr)
        // Attach the column name so a parse failure points at the offending field.
        .with_context(|| format!("Invalid value for column '{}'", property.name))?;
    coerce_literal(property, &literal)
}

/// Coerces an already-extracted [`Literal`] into the Notion JSON shape for one property.
///
/// This is the core type-directed dispatch: the same literal becomes a different
/// JSON shape depending on `property.property_type`, which is why schema
/// resolution must precede this call. `NULL` is special-cased up front and routed
/// to `clear_property_value`, because clearing a property uses a distinct
/// payload (often an empty array or `null`) rather than a value payload.
///
/// # Parameters
///
/// - `property`: the resolved schema whose `property_type` selects the shape.
/// - `literal`: the value to embed in that shape.
///
/// # Errors
///
/// Returns an error if the literal cannot be coerced to the required scalar type
/// (propagated from the `as_*` helpers), or if the property type is
/// [`PropertyType::Unsupported`].
pub fn coerce_literal(property: &PropertySchema, literal: &Literal) -> Result<JsonValue> {
    // NULL never produces a value payload; it means "clear", handled separately.
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

/// Builds the Notion payload that clears one property in response to SQL `NULL`.
///
/// Each property type clears differently: text/multi-select clear to an empty
/// array, while number/select/date clear to `null`. Some types have no valid
/// "empty" representation in Notion and therefore reject `NULL`:
/// - `Title` and `Status` are required by Notion and cannot be emptied.
/// - `Checkbox` is strictly boolean; there is no null checkbox, so clearing it is
///   meaningless (use an explicit `false` instead).
///
/// # Parameters
///
/// - `property`: the resolved schema for the column being cleared.
///
/// # Errors
///
/// Returns an error for property types that cannot be cleared with `NULL`
/// (`Title`, `Checkbox`, `Status`) and for [`PropertyType::Unsupported`].
fn clear_property_value(property: &PropertySchema) -> Result<JsonValue> {
    match property.property_type {
        // Title is mandatory in Notion; there is no empty-title representation.
        PropertyType::Title => bail!("Title properties cannot be cleared with NULL"),
        PropertyType::RichText => Ok(json!({ "rich_text": [] })),
        PropertyType::Number => Ok(json!({ "number": null })),
        // A checkbox is always true/false; "clear" has no meaning, so reject it.
        PropertyType::Checkbox => bail!("Checkbox properties cannot be cleared with NULL"),
        PropertyType::Select => Ok(json!({ "select": null })),
        // Status must reference a defined option and cannot be set to nothing.
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

/// Renders an `f64` as text, dropping the fractional part for integral values.
///
/// Used by [`Literal::as_string`] so a whole number like `42.0` becomes `"42"`
/// rather than `"42"`-with-a-trailing-`.0`, which is what a user writing an
/// integer literal expects to land in a text property. Non-integral values fall
/// back to the default `f64` formatting.
///
/// # Parameters
///
/// - `value`: the number to format.
///
/// # Returns
///
/// The textual form of `value`, without a `.0` suffix when it is integral.
fn number_to_string(value: f64) -> String {
    // `fract() == 0.0` detects integral values; format with zero decimals to drop ".0".
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        value.to_string()
    }
}
