// SPDX-License-Identifier: MIT
// Copyright (c) 2026 James Maes
//! SQL `WHERE` expression translation for Notion database filters.
//!
//! `sqlparser` keeps SQL expressions in a database-neutral AST. This module
//! narrows that AST to the comparison forms that the Notion filter API can
//! represent for the supported property types.
//!
//! # Responsibilities
//!
//! - Walk a `sqlparser` [`Expr`] tree and emit the JSON filter object that
//!   Notion's database query endpoint expects.
//! - Reject any SQL construct that has no faithful Notion equivalent, failing
//!   loudly rather than silently producing a filter with different semantics.
//!
//! # Design
//!
//! The Notion filter model is property-type-specific: the same SQL operator
//! maps to a different JSON key depending on whether the column is text, a
//! number, a date, and so on. To keep the SQL-side parsing separate from the
//! Notion-side encoding, every supported operator is first normalized into the
//! internal `ComparisonOp` enum, and the per-type `*_condition` helpers then
//! decide which Notion conditions are legal for that type. This two-stage
//! design is why operators such as `LIKE` (which Notion lacks) can be rewritten
//! into `contains` / `starts_with` / `ends_with` conditions before they ever
//! reach a property encoder.
//!
//! # Key items
//!
//! - [`translate_where`] is the single public entry point.
//! - `ComparisonOp` is the operator-normalization layer.
//! - The `*_condition` functions encode one `ComparisonOp` for one property
//!   type, and are the authoritative list of what each Notion type supports.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value as JsonValue};
use sqlparser::ast::{BinaryOperator, Expr};

use crate::schema::{DatabaseSchema, PropertySchema, PropertyType};
use crate::value::{literal_from_expr, Literal};

/// Internal comparison model shared by SQL operators and Notion filter conditions.
///
/// This enum is the normalization point between two vocabularies: SQL operators
/// (`=`, `LIKE`, ...) on one side and Notion filter conditions (`equals`,
/// `contains`, ...) on the other. Decoupling the two means the `LIKE` rewriter
/// and the per-property-type encoders only ever speak in these variants, never
/// in raw `sqlparser` operators.
///
/// It is `Copy` because it is a trivial fieldless enum that is passed by value
/// through the encoding helpers.
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
///
/// This is the public entry point and the recursive core of the module. It
/// walks the `sqlparser` AST top-down: boolean `AND`/`OR` nodes recurse and are
/// wrapped in Notion's `{"and": [...]}` / `{"or": [...]}` compound filters,
/// while leaf comparisons are resolved against the schema and encoded by
/// `property_filter`.
///
/// # Parameters
///
/// - `expr`: the `WHERE` sub-expression to translate; for a top-level call this
///   is the whole `WHERE` clause, and recursive calls pass the operands of
///   boolean and nested nodes.
/// - `schema`: the database schema used to resolve column references to their
///   Notion property type, which determines the legal comparisons.
///
/// # Returns
///
/// A [`JsonValue`] holding the Notion filter object equivalent to `expr`.
///
/// # Errors
///
/// Returns an error if `expr` contains a SQL construct with no faithful Notion
/// equivalent (e.g. `LIKE ANY`, `LIKE ESCAPE`, `NOT IN`, an unrecognized
/// expression form), if a column cannot be resolved against `schema`, if a
/// comparison value is not a valid literal, or if the column's property type
/// does not support the requested comparison.
pub fn translate_where(expr: &Expr, schema: &DatabaseSchema) -> Result<JsonValue> {
    match expr {
        // `AND` / `OR` recurse on both operands and wrap them in Notion's
        // compound-filter form. They are matched ahead of the generic
        // `BinaryOp` arm so the boolean operators never fall through to the
        // comparison encoder, which would reject them.
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
        // Any remaining binary operator is treated as a `column <op> value`
        // comparison: the left operand must be a column and the right a literal.
        Expr::BinaryOp { left, op, right } => {
            let comparison = comparison_from_binary_operator(op)?;
            let property = resolve_column(left, schema)?;
            let literal = literal_from_expr(right)
                .with_context(|| format!("Invalid comparison value for '{}'", property.name))?;
            property_filter(property, comparison, &literal)
        }
        // `LIKE` and `ILIKE` share identical fields and rewrite logic, so they
        // are handled by one arm. Notion does case-insensitive substring
        // matching anyway, so the distinction is not preserved on output.
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
            // `LIKE ANY (...)` and custom escape characters have no Notion
            // analogue; reject them rather than silently dropping semantics.
            if *any {
                bail!("LIKE ANY is not supported");
            }
            if escape_char.is_some() {
                bail!("LIKE ESCAPE clauses are not supported");
            }
            let property = resolve_column(expr, schema)?;
            let literal = literal_from_expr(pattern)
                .with_context(|| format!("Invalid LIKE pattern for '{}'", property.name))?;
            // The wildcard pattern is reduced to a comparison op and a stripped
            // value, then re-wrapped as a plain string literal so the normal
            // property encoder can consume it.
            let (comparison, value) = like_pattern_to_comparison(&literal.as_string()?, *negated)?;
            let literal = Literal::String(value);
            property_filter(property, comparison, &literal)
        }
        Expr::InList {
            expr,
            list,
            negated,
        } => {
            // `NOT IN` would require an AND-of-not-equals chain; it is rejected
            // for now rather than approximated.
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
        // `IS NULL` / `IS NOT NULL` map onto Notion's `is_empty` /
        // `is_not_empty` conditions, which apply uniformly across property types.
        Expr::IsNull(expr) => {
            let property = resolve_column(expr, schema)?;
            Ok(empty_filter(property, true))
        }
        Expr::IsNotNull(expr) => {
            let property = resolve_column(expr, schema)?;
            Ok(empty_filter(property, false))
        }
        // Parenthesized groups carry no semantics of their own; unwrap and recurse.
        Expr::Nested(expr) => translate_where(expr, schema),
        other => bail!("Unsupported WHERE expression '{other}'"),
    }
}

/// Resolves an expression that must be a column reference into a database property.
///
/// # Parameters
///
/// - `expr`: the expression expected to denote a column (the left side of a
///   comparison, or the subject of `LIKE`/`IN`/`IS NULL`).
/// - `schema`: the schema the resolved name is looked up in.
///
/// # Returns
///
/// A borrowed [`PropertySchema`] for the named column. The returned reference
/// is tied to the lifetime of `schema`.
///
/// # Errors
///
/// Returns an error if `expr` is not a bare column name (see [`column_name`])
/// or if the name does not match any property in `schema`.
fn resolve_column<'a>(expr: &Expr, schema: &'a DatabaseSchema) -> Result<&'a PropertySchema> {
    let column = column_name(expr)?;
    schema.resolve_property(&column)
}

/// Extracts an unqualified column name from a SQL expression.
///
/// # Parameters
///
/// - `expr`: the expression to interpret as a column reference.
///
/// # Returns
///
/// The bare column name as an owned `String`.
///
/// # Errors
///
/// Returns an error for qualified names (`table.column`), which Notion has no
/// concept of since a query targets a single database, and for any expression
/// that is not an identifier at all.
fn column_name(expr: &Expr) -> Result<String> {
    match expr {
        Expr::Identifier(ident) => Ok(ident.value.clone()),
        // A single-segment compound identifier is just a bare name wrapped in
        // the compound form; accept it. Anything longer is a qualified name.
        Expr::CompoundIdentifier(parts) if parts.len() == 1 => Ok(parts[0].value.clone()),
        Expr::CompoundIdentifier(_) => bail!("Qualified column names are not supported"),
        other => bail!("Expected a column name, got '{other}'"),
    }
}

/// Converts supported SQL binary operators into the internal comparison enum.
///
/// Note that `AND`/`OR` are intentionally absent: they are handled as compound
/// filters in [`translate_where`] before reaching this function, so reaching
/// here with a boolean operator is a caller error and yields an error result.
///
/// # Parameters
///
/// - `op`: the SQL binary operator from a comparison node.
///
/// # Returns
///
/// The equivalent `ComparisonOp`.
///
/// # Errors
///
/// Returns an error for any operator outside the comparison set (`=`, `<>`,
/// `>`, `<`, `>=`, `<=`), such as arithmetic or boolean operators.
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
///
/// Dispatches on the property's Notion type to the matching `*_condition`
/// encoder, then wraps the resulting condition under the type's Notion key
/// (e.g. `{"property": name, "rich_text": {...}}`).
///
/// # Parameters
///
/// - `property`: the resolved schema entry, supplying both the Notion property
///   name and its type.
/// - `op`: the normalized comparison to encode.
/// - `literal`: the right-hand-side value; each encoder coerces it to the type
///   it needs (string, number, or bool).
///
/// # Returns
///
/// A complete single-condition Notion filter object.
///
/// # Errors
///
/// Returns an error if the property has an [`PropertyType::Unsupported`] type,
/// or if the selected encoder rejects `op` or fails to coerce `literal`.
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
///
/// Text is the richest type: it supports equality, inequality, substring, and
/// affix matching, which is what makes it the target for rewritten `LIKE`
/// patterns.
///
/// # Parameters
///
/// - `op`: the comparison to encode.
/// - `literal`: the comparison value, coerced to a string.
///
/// # Returns
///
/// The inner Notion condition object (the value under the property's type key).
///
/// # Errors
///
/// Returns an error if `literal` is not string-coercible, or if `op` is a
/// numeric/ordering comparison (`>`, `<`, ...) that text properties do not
/// support.
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
///
/// Select and status are single-value enumerations, so only equality and
/// inequality against an option name are meaningful.
///
/// # Parameters
///
/// - `op`: the comparison to encode.
/// - `literal`: the option name, coerced to a string.
///
/// # Returns
///
/// The inner Notion condition object.
///
/// # Errors
///
/// Returns an error if `literal` is not string-coercible, or if `op` is
/// anything other than `=` / `<>`.
fn select_condition(op: ComparisonOp, literal: &Literal) -> Result<JsonValue> {
    let value = literal.as_string()?;
    match op {
        ComparisonOp::Eq => Ok(json!({ "equals": value })),
        ComparisonOp::NotEq => Ok(json!({ "does_not_equal": value })),
        other => bail!("Select/status properties do not support {other:?} comparisons"),
    }
}

/// Builds a Notion multi-select condition.
///
/// A multi-select holds a set of options, so SQL equality is interpreted as
/// set membership: `=` and `LIKE`-derived `Contains` both map to Notion's
/// `contains`, and `<>` / `DoesNotContain` both map to `does_not_contain`.
///
/// # Parameters
///
/// - `op`: the comparison to encode.
/// - `literal`: the option name to test for membership, coerced to a string.
///
/// # Returns
///
/// The inner Notion condition object.
///
/// # Errors
///
/// Returns an error if `literal` is not string-coercible, or if `op` is an
/// ordering comparison that has no set-membership meaning.
fn multi_select_condition(op: ComparisonOp, literal: &Literal) -> Result<JsonValue> {
    let value = literal.as_string()?;
    match op {
        // Both `= 'x'` and `LIKE '%x%'` are treated as "the set contains x".
        ComparisonOp::Eq | ComparisonOp::Contains => Ok(json!({ "contains": value })),
        ComparisonOp::NotEq | ComparisonOp::DoesNotContain => {
            Ok(json!({ "does_not_contain": value }))
        }
        other => bail!("Multi-select properties do not support {other:?} comparisons"),
    }
}

/// Builds a Notion number condition.
///
/// Numbers support the full set of equality and ordering comparisons, each
/// mapping to a distinct Notion key.
///
/// # Parameters
///
/// - `op`: the comparison to encode.
/// - `literal`: the comparison value, coerced to a number.
///
/// # Returns
///
/// The inner Notion condition object.
///
/// # Errors
///
/// Returns an error if `literal` is not numeric, or if `op` is a text-only
/// comparison (`Contains`, `StartsWith`, ...).
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
///
/// A checkbox is a boolean, so only equality and inequality against `true` /
/// `false` apply.
///
/// # Parameters
///
/// - `op`: the comparison to encode.
/// - `literal`: the comparison value, coerced to a bool.
///
/// # Returns
///
/// The inner Notion condition object.
///
/// # Errors
///
/// Returns an error if `literal` is not boolean-coercible, or if `op` is
/// anything other than `=` / `<>`.
fn checkbox_condition(op: ComparisonOp, literal: &Literal) -> Result<JsonValue> {
    let value = literal.as_bool()?;
    match op {
        ComparisonOp::Eq => Ok(json!({ "equals": value })),
        ComparisonOp::NotEq => Ok(json!({ "does_not_equal": value })),
        other => bail!("Checkbox properties do not support {other:?} comparisons"),
    }
}

/// Builds a Notion date condition.
///
/// Dates support equality and ordering, but Notion uses temporal vocabulary
/// (`after`, `before`, `on_or_after`, `on_or_before`) rather than the numeric
/// `greater_than`/`less_than` keys. The value is passed through as a string
/// (an ISO-8601 date/datetime) without further parsing here.
///
/// # Parameters
///
/// - `op`: the comparison to encode.
/// - `literal`: the date value, coerced to a string.
///
/// # Returns
///
/// The inner Notion condition object.
///
/// # Errors
///
/// Returns an error if `literal` is not string-coercible, or if `op` is a
/// text-only comparison (`Contains`, `StartsWith`, ...).
fn date_condition(op: ComparisonOp, literal: &Literal) -> Result<JsonValue> {
    let value = literal.as_string()?;
    match op {
        ComparisonOp::Eq => Ok(json!({ "equals": value })),
        ComparisonOp::NotEq => Ok(json!({ "does_not_equal": value })),
        // Ordering uses Notion's temporal key names, not numeric ones.
        ComparisonOp::Gt => Ok(json!({ "after": value })),
        ComparisonOp::Lt => Ok(json!({ "before": value })),
        ComparisonOp::GtEq => Ok(json!({ "on_or_after": value })),
        ComparisonOp::LtEq => Ok(json!({ "on_or_before": value })),
        other => bail!("Date properties do not support {other:?} comparisons"),
    }
}

/// Builds an `IS NULL` or `IS NOT NULL` Notion filter for a property.
///
/// Unlike the `*_condition` helpers, this returns a complete filter object
/// (including the `property` key) because the `is_empty`/`is_not_empty`
/// condition is identical across every property type and needs no per-type
/// dispatch or literal coercion. It is therefore infallible and returns a
/// plain [`JsonValue`] rather than a `Result`.
///
/// # Parameters
///
/// - `property`: the column whose presence is being tested.
/// - `is_empty`: `true` encodes `IS NULL` (`is_empty`), `false` encodes
///   `IS NOT NULL` (`is_not_empty`).
///
/// # Returns
///
/// A complete single-condition Notion filter object.
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
///
/// Notion has no `LIKE` operator, so a constrained subset of SQL patterns is
/// rewritten into the substring/affix conditions Notion does provide. Only
/// leading and/or trailing `%` wildcards are accepted; every other wildcard
/// form is rejected because it cannot be expressed losslessly.
///
/// The mapping of stripped pattern to comparison is:
///
/// ```text
/// 'value'    -> Eq          (no wildcards: exact match)
/// 'value%'   -> StartsWith  (trailing % only)
/// '%value'   -> EndsWith    (leading % only)
/// '%value%'  -> Contains    (both ends)
/// ```
///
/// The returned `String` is the literal text with its surrounding `%` removed,
/// ready to be wrapped as a [`Literal::String`] by the caller.
///
/// # Parameters
///
/// - `pattern`: the raw `LIKE` pattern string (still containing any `%`).
/// - `negated`: whether the source expression was `NOT LIKE`.
///
/// # Returns
///
/// A tuple of the resolved `ComparisonOp` and the wildcard-stripped value.
///
/// # Errors
///
/// Returns an error if the pattern uses an unsupported wildcard construct
/// (escape sequences, `_`, wildcard-only patterns, or interior `%`), or if it
/// is a `NOT LIKE` prefix/suffix pattern — Notion has no negated `starts_with`
/// or `ends_with`, so those combinations cannot be represented.
///
/// # Panics
///
/// Does not panic in practice. The final `unreachable!` guards an invariant:
/// the pattern-analysis `match` above only ever produces `Eq`, `StartsWith`,
/// `EndsWith`, or `Contains`, so the negation `match` cannot encounter any
/// other operator.
fn like_pattern_to_comparison(pattern: &str, negated: bool) -> Result<(ComparisonOp, String)> {
    // Backslash escapes and `_` (single-character wildcard) have no Notion
    // equivalent, so reject patterns that rely on them.
    if pattern.contains('\\') {
        bail!("Escaped LIKE wildcard patterns are not supported");
    }
    if pattern.contains('_') {
        bail!("LIKE '_' wildcards are not supported");
    }

    let percent_count = pattern.chars().filter(|value| *value == '%').count();
    // A pattern that is nothing but `%` characters matches everything, which is
    // a degenerate filter; reject it rather than emit a meaningless condition.
    if percent_count > 0 && pattern.chars().all(|value| value == '%') {
        bail!("LIKE wildcard-only patterns are not supported");
    }

    let starts_with_percent = pattern.starts_with('%');
    let ends_with_percent = pattern.ends_with('%');

    // Classify by (leading %, trailing %, total % count). The count guards
    // against interior wildcards: e.g. `a%b` has count 1 but neither end is a
    // `%`, so it falls through to the catch-all error arm. `%a%b%` has count 3
    // and likewise fails, leaving only the four supported shapes.
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

    // Fold negation into the comparison. Notion can negate equality and
    // containment, but not the affix conditions, so `NOT LIKE` prefix/suffix
    // patterns are rejected here.
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
        // Unreachable: the analysis above never emits any other operator.
        _ => unreachable!("LIKE pattern analysis only emits LIKE comparison operators"),
    }
}
