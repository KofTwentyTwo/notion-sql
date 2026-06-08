//! Integration tests for SQL `WHERE` to Notion query filter translation.
//!
//! These tests exercise [`notion_sql::filter::translate_where`], the function
//! that converts a parsed SQL boolean expression (the `WHERE` clause) into the
//! JSON filter object that the Notion "query a database" API expects. The crate
//! lets callers query a Notion database using SQL-ish syntax; this file pins
//! down the exact shape of the translation for the supported predicate forms.
//!
//! # Testing approach
//!
//! Each test follows the same pattern:
//! 1. Build a fixed [`DatabaseSchema`] via [`schema`] so column names resolve to
//!    known Notion property types (the property type dictates which filter
//!    operators are legal, e.g. `status` vs `number` vs `date`).
//! 2. Parse a SQL fragment into an [`sqlparser::ast::Expr`] via [`where_expr`].
//! 3. Translate it and assert on the resulting `serde_json::Value`, comparing
//!    against a hand-written `json!` literal that mirrors the documented Notion
//!    filter format exactly.
//!
//! The behaviors under test cover: boolean grouping (`AND`/`OR` nesting),
//! the supported and explicitly-rejected `LIKE`/`ILIKE` patterns, `IN` lists,
//! `IS [NOT] NULL` checks, date comparisons, and case-insensitive column-name
//! matching. Because Notion filters are deeply nested JSON, the assertions are
//! intentionally written as full literals rather than partial checks so that
//! any structural drift in the translator is caught.

use serde_json::json;
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;

use notion_sql::{filter::translate_where, schema::DatabaseSchema};

/// Builds the fixed [`DatabaseSchema`] fixture shared by every test.
///
/// The schema declares one column of each Notion property type that the tests
/// need so that [`translate_where`] can resolve a column name to its property
/// type and pick the correct filter operator family. The property names and
/// types here are the source of truth the assertions are written against:
/// `Name`/title, `Status`/status, `Tags`/multi_select, `Priority`/number,
/// `Done`/checkbox, `Due`/date.
///
/// # Returns
///
/// A fully-parsed [`DatabaseSchema`] mirroring a Notion database `properties`
/// payload.
///
/// # Panics
///
/// Panics (via `unwrap`) if [`DatabaseSchema::from_notion_database`] rejects the
/// literal — that would indicate a bug in the fixture itself, so failing loudly
/// is the desired behavior in a test.
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

/// Parses a SQL `WHERE`-clause fragment into its AST expression node.
///
/// The translator under test operates on a single boolean [`Expr`], not on a
/// whole statement, so this helper wraps the fragment in a minimal
/// `SELECT * FROM Tasks WHERE <sql>` statement, parses it with the permissive
/// [`GenericDialect`], and digs the `selection` (the `WHERE` predicate) back out
/// of the resulting AST. The table name `Tasks` is arbitrary — only the
/// predicate is used downstream.
///
/// [`Expr`]: sqlparser::ast::Expr
///
/// # Parameters
///
/// - `sql`: the `WHERE`-clause body without the `WHERE` keyword, e.g.
///   `"Status = 'Done'"`.
///
/// # Returns
///
/// The parsed predicate as an [`sqlparser::ast::Expr`].
///
/// # Panics
///
/// Panics if the fragment fails to parse, if the parse yields no statement, or
/// if the statement is not a `SELECT` carrying a `WHERE` clause. All of these
/// indicate a malformed test input rather than a translator bug, so panicking
/// surfaces the mistake immediately.
fn where_expr(sql: &str) -> sqlparser::ast::Expr {
    let parsed = Parser::parse_sql(
        &GenericDialect {},
        &format!("SELECT * FROM Tasks WHERE {sql}"),
    )
    .unwrap();
    let statement = parsed.into_iter().next().unwrap();
    // Walk the AST down to the SELECT body and pull out its `WHERE` predicate.
    // `selection` is `None` only when there is no WHERE clause, which the
    // wrapper guarantees we always have, so the inner `unwrap` is safe here.
    if let sqlparser::ast::Statement::Query(query) = statement {
        if let sqlparser::ast::SetExpr::Select(select) = query.body.as_ref() {
            return select.selection.clone().unwrap();
        }
    }
    // Reached only if the AST shape is unexpectedly not a plain SELECT.
    panic!("failed to extract WHERE expression")
}

/// Verifies that `AND`/`OR` with explicit parenthesised grouping maps onto
/// Notion's nested `and`/`or` compound-filter arrays.
///
/// The input mixes a top-level `AND` with a parenthesised `OR`, exercising the
/// translator's ability to (a) preserve operator precedence/grouping and (b)
/// emit the correct per-property operator for each leaf (`status.equals`,
/// `number.greater_than_or_equal_to`, `checkbox.equals`). The `2` literal is
/// asserted as `2.0` because numbers are normalised to JSON floats.
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

/// Pins down the mapping from the *supported* `LIKE`/`ILIKE` wildcard shapes to
/// Notion's text operators.
///
/// Each case asserts one shape: a bare literal becomes `equals`, a trailing `%`
/// becomes `starts_with`, a leading `%` becomes `ends_with`, both becomes
/// `contains`, and the negated `NOT LIKE` forms produce the corresponding
/// `does_not_*` operators. `ILIKE` is accepted alongside `LIKE`; the translator
/// does not distinguish case-sensitivity here because Notion text matching is
/// already case-insensitive. The rejected shapes are covered separately by
/// [`rejects_unsupported_like_patterns`].
#[test]
fn translates_supported_like_patterns() {
    // (sql_fragment, expected_notion_filter) — driven as a table so each
    // wildcard shape is asserted independently in the loop below.
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

    // Translate each fragment and assert it matches its expected filter.
    for (sql, expected) in cases {
        assert_eq!(
            translate_where(&where_expr(sql), &schema()).unwrap(),
            expected
        );
    }
}

/// Confirms that `LIKE`/`ILIKE` shapes Notion cannot express are rejected with a
/// descriptive error rather than silently mistranslated.
///
/// Notion only supports prefix/suffix/contains/equals text matching, so wildcard
/// shapes such as an interior `%` (`'ta%sk'`), `_` single-char wildcards, escaped
/// wildcards, explicit `ESCAPE` clauses, a wildcard-only pattern (`'%'`), and
/// negated prefix/suffix forms have no faithful translation. Each case asserts
/// the error message *contains* a human-readable fragment naming the
/// unsupported feature; substring matching keeps the test robust to surrounding
/// wording while still verifying the right rejection reason fired.
#[test]
fn rejects_unsupported_like_patterns() {
    // (sql_fragment, expected_error_substring) for each unsupported shape.
    let cases = [
        ("Name LIKE 'ta%sk'", "leading and trailing '%'"),
        ("Name LIKE 'ta_sk'", "LIKE '_' wildcards"),
        ("Name LIKE 'ta\\%sk'", "Escaped LIKE wildcard patterns"),
        ("Name LIKE 'ta#%sk' ESCAPE '#'", "LIKE ESCAPE clauses"),
        ("Name LIKE '%'", "wildcard-only patterns"),
        ("Name NOT LIKE 'task%'", "NOT LIKE prefix patterns"),
        ("Name NOT LIKE '%task'", "NOT LIKE suffix patterns"),
    ];

    // Each fragment must translate to an `Err`; `unwrap_err` panics (failing
    // the test) if the translator unexpectedly accepted an unsupported pattern.
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

/// Verifies that a SQL `IN (...)` list against a `multi_select` column expands
/// into an `or` of `contains` filters — one per list element.
///
/// Notion has no native "value in set" operator, so membership is modelled as a
/// disjunction. The test asserts both the `or` wrapper and the per-element
/// `multi_select.contains` leaves, and implicitly that list order is preserved.
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

/// Covers two date-column behaviors: null checks and ordered comparisons.
///
/// `IS NOT NULL` maps to Notion's emptiness operator (`date.is_not_empty`),
/// which is the inverse of `is_empty` — Notion expresses presence/absence via a
/// boolean flag rather than a NULL sentinel. A `<` comparison against a date
/// literal maps to `date.before`, with the date string passed through verbatim
/// (no reformatting). Both are asserted in one test because they share the date
/// property type and reinforce that the same column supports multiple operator
/// families.
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

/// Verifies that column references resolve to schema properties regardless of
/// case.
///
/// The SQL uses lowercase `status` while the schema property is `Status`; the
/// translator must still resolve the column and, importantly, emit the
/// canonical property name (`"Status"`) from the schema in the output filter —
/// Notion's API keys on the exact stored property name, so the original casing
/// must be restored rather than echoed from the query.
#[test]
fn matches_column_names_case_insensitively() {
    let filter = translate_where(&where_expr("status = 'Done'"), &schema()).unwrap();
    assert_eq!(
        filter,
        json!({ "property": "Status", "status": { "equals": "Done" } })
    );
}
