// SPDX-License-Identifier: MIT
// Copyright (c) 2026 James Maes
//! SQL parsing and validation for the subset supported by `notion-sql`.
//!
//! `sqlparser` accepts a broad SQL grammar. This module constrains that AST to
//! simple single-table CRUD operations that can be translated to Notion.
//!
//! # Responsibilities
//!
//! - Parse one SQL string with [`parse_statement`] and reduce the rich
//!   `sqlparser` AST down to the narrow [`SqlStatement`] enum the rest of the
//!   crate understands.
//! - Reject, with a descriptive error, every SQL construct that has no faithful
//!   Notion equivalent (joins, subqueries, set operations, `RETURNING`,
//!   `OFFSET`, aliases, qualified columns, and so on). The guiding principle is
//!   "fail loudly rather than silently translate into something subtly wrong":
//!   a query that parses here is guaranteed to map cleanly onto a single Notion
//!   database operation.
//!
//! # Key types
//!
//! - [`SqlStatement`] — the validated, Notion-translatable statement shapes
//!   (`Select`, `Insert`, `Update`, `Delete`).
//! - [`SelectColumns`], [`SortSpec`], [`Assignment`] — the supporting pieces of
//!   a `SELECT`/`UPDATE` that downstream code consumes.
//!
//! # Place in the crate
//!
//! This module is purely a front end: it produces [`SqlStatement`] values and
//! leaves remaining work — resolving column/database names against the live
//! Notion schema and turning literal [`Expr`]s into concrete values (see
//! [`crate::value`]) — to later stages. Column and database names are therefore
//! carried through as raw strings and validated/resolved elsewhere.

use anyhow::{anyhow, bail, Context, Result};
use sqlparser::ast::{
    AssignmentTarget, Expr, FromTable, FunctionArg, FunctionArgExpr, FunctionArguments, Ident,
    LimitClause, ObjectName, ObjectNamePart, OrderByKind, Query, SelectItem, SetExpr, Statement,
    TableFactor, TableObject, TableWithJoins, Value,
};
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;

/// Projection requested by a `SELECT` statement.
///
/// This is intentionally a small closed set: Notion can return all properties,
/// a chosen subset of properties, or a single row count. Anything more complex
/// (expressions, aliases, multiple aggregates) is rejected during parsing, so
/// downstream code only ever has to handle these three cases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectColumns {
    /// `SELECT *`, resolved later through the Notion schema.
    ///
    /// The concrete property list is unknown until the database schema is
    /// fetched, so the wildcard is preserved verbatim rather than expanded here.
    All,
    /// Explicit column names in SQL projection order.
    ///
    /// Order is preserved so result columns can be emitted in the order the
    /// user wrote them. Names are raw and resolved against the schema later.
    Columns(Vec<String>),
    /// `SELECT COUNT(*)` or `SELECT COUNT(1)`, rendered as a one-row aggregate.
    Count,
}

/// One parsed `ORDER BY` item.
///
/// A `SELECT` may carry several of these; their order in the vector is the sort
/// priority, mirroring SQL semantics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SortSpec {
    /// Column to sort by, resolved later through the Notion schema.
    pub column: String,
    /// `true` for ascending order and `false` for descending order.
    ///
    /// SQL's default (no `ASC`/`DESC` keyword) is ascending, which the parser
    /// encodes as `true`.
    pub ascending: bool,
}

/// One parsed `UPDATE SET` assignment.
///
/// The right-hand side is kept as a raw [`Expr`] rather than an evaluated value
/// because turning expressions into concrete Notion property values is the job
/// of [`crate::value`], which needs the target property's type to interpret a
/// literal correctly.
#[derive(Debug, Clone, PartialEq)]
pub struct Assignment {
    /// Column receiving the assignment.
    pub column: String,
    /// SQL expression for the assigned literal value.
    pub value: Expr,
}

/// Supported SQL statement shapes after validation.
///
/// Every variant maps to exactly one operation against a single Notion
/// database. `WHERE` filters are carried as raw [`Expr`]s because translating
/// them into Notion's filter API requires the database schema, which is not
/// available at parse time. The `database` field is the name or ID written in
/// the SQL `FROM`/`INTO`/target clause; it is resolved to a real Notion
/// database later.
#[derive(Debug, Clone, PartialEq)]
pub enum SqlStatement {
    /// A single-database `SELECT` query.
    Select {
        /// Database name or ID from the `FROM` clause.
        database: String,
        /// Projection requested by the query.
        columns: SelectColumns,
        /// Optional `WHERE` expression.
        filter: Option<Expr>,
        /// Parsed sort specifications.
        ///
        /// Empty when the query has no `ORDER BY`.
        sorts: Vec<SortSpec>,
        /// Optional row limit.
        ///
        /// `None` means unbounded; validated to be a non-negative integer.
        limit: Option<usize>,
    },
    /// An `INSERT INTO ... VALUES` statement.
    Insert {
        /// Database name or ID from the target table.
        database: String,
        /// Target columns receiving values.
        ///
        /// Required: the column list is mandatory so each value can be matched
        /// to a named property (Notion has no implicit column order).
        columns: Vec<String>,
        /// Values grouped by row.
        ///
        /// Each inner vector is one row; cell count is not checked here against
        /// `columns`, that alignment is validated downstream with the schema.
        rows: Vec<Vec<Expr>>,
    },
    /// An `UPDATE` statement.
    Update {
        /// Database name or ID from the update target.
        database: String,
        /// Property assignments to apply to matched pages.
        assignments: Vec<Assignment>,
        /// Optional `WHERE` expression.
        ///
        /// A missing filter updates every page in the database.
        filter: Option<Expr>,
    },
    /// A `DELETE` statement, implemented as moving pages to trash.
    ///
    /// Notion has no hard delete via the API, so this is realized by trashing
    /// (archiving) every matched page.
    Delete {
        /// Database name or ID from the delete target.
        database: String,
        /// Optional `WHERE` expression.
        ///
        /// A missing filter trashes every page in the database.
        filter: Option<Expr>,
    },
}

/// Parses one SQL string into a supported statement representation.
///
/// This is the module's single entry point. It parses with the permissive
/// [`GenericDialect`] and then narrows the resulting AST to one of the
/// [`SqlStatement`] variants, rejecting any unsupported construct along the way.
///
/// # Parameters
///
/// - `sql`: the raw SQL text; must contain exactly one statement.
///
/// # Returns
///
/// The validated [`SqlStatement`] on success.
///
/// # Errors
///
/// Returns an error if the text fails to parse, if it contains zero or more
/// than one statement, if the statement type is unsupported (e.g. `CREATE`,
/// `DROP`), or if any nested clause is one this crate cannot translate to
/// Notion (joins, `RETURNING`, `USING`, `UPDATE ... FROM`, `DELETE` with
/// `ORDER BY`/`LIMIT`, etc.).
///
/// # Panics
///
/// Does not panic in practice: the internal `unwrap` on the sole statement is
/// guarded by the preceding length-equals-one check.
pub fn parse_statement(sql: &str) -> Result<SqlStatement> {
    let dialect = GenericDialect {};
    let statements = Parser::parse_sql(&dialect, sql).context("Failed to parse SQL")?;
    // Reject empty input and multi-statement scripts: each call maps to one
    // Notion operation, so ambiguity here would be silently lossy.
    if statements.len() != 1 {
        bail!("Expected exactly one SQL statement");
    }

    // `unwrap` is safe: the length check above guarantees exactly one element.
    match statements.into_iter().next().unwrap() {
        Statement::Query(query) => parse_select(*query),
        Statement::Insert(insert) => parse_insert(insert),
        Statement::Update {
            table,
            assignments,
            selection,
            from,
            returning,
            limit,
            ..
        } => {
            // Notion page updates only support changing properties on matched
            // pages, so SQL clauses that imply joined or returned rows are rejected.
            if from.is_some() {
                bail!("UPDATE ... FROM is not supported");
            }
            if returning.is_some() {
                bail!("UPDATE ... RETURNING is not supported");
            }
            if limit.is_some() {
                bail!("UPDATE ... LIMIT is not supported");
            }
            Ok(SqlStatement::Update {
                database: table_name_from_table_with_joins(&table)?,
                assignments: assignments
                    .into_iter()
                    .map(|assignment| {
                        Ok(Assignment {
                            column: assignment_target_name(&assignment.target)?,
                            value: assignment.value,
                        })
                    })
                    .collect::<Result<Vec<_>>>()?,
                filter: selection,
            })
        }
        Statement::Delete(delete) => {
            if delete.using.is_some() {
                bail!("DELETE ... USING is not supported");
            }
            // Deletion is implemented as trashing all matched pages. Returning,
            // ordering, and limiting would imply SQL behavior Notion cannot mirror.
            if delete.returning.is_some() {
                bail!("DELETE ... RETURNING is not supported");
            }
            if !delete.order_by.is_empty() || delete.limit.is_some() {
                bail!("DELETE ORDER BY/LIMIT is not supported");
            }
            Ok(SqlStatement::Delete {
                database: table_name_from_from_table(&delete.from)?,
                filter: delete.selection,
            })
        }
        other => bail!("Unsupported SQL statement '{other}'"),
    }
}

/// Parses and validates a simple `SELECT` query.
///
/// Accepts only projection + `FROM` + `WHERE` + `ORDER BY` + `LIMIT`; every
/// other query feature is rejected so the result is guaranteed translatable.
///
/// # Parameters
///
/// - `query`: the `sqlparser` query AST taken from a `Statement::Query`.
///
/// # Returns
///
/// A [`SqlStatement::Select`].
///
/// # Errors
///
/// Returns an error for `WITH`/CTEs, `FETCH`, locking clauses, set operations
/// or nested query bodies, and `SELECT` modifiers with no Notion analogue
/// (`DISTINCT`, `TOP`, `INTO`, lateral views, `PREWHERE`, `HAVING`, `QUALIFY`).
/// Also errors when `COUNT` is combined with `ORDER BY` or `LIMIT`, since a
/// single-row count cannot be sorted or limited meaningfully.
fn parse_select(query: Query) -> Result<SqlStatement> {
    let Query {
        body,
        order_by,
        limit_clause,
        with,
        fetch,
        locks,
        ..
    } = query;

    if with.is_some() {
        bail!("WITH queries are not supported");
    }
    if fetch.is_some() || !locks.is_empty() {
        bail!("FETCH and locking clauses are not supported");
    }

    // Set operations and nested query bodies have no direct Notion equivalent.
    let select = match *body {
        SetExpr::Select(select) => *select,
        other => bail!("Only simple SELECT queries are supported, got '{other}'"),
    };

    if select.distinct.is_some()
        || select.top.is_some()
        || select.into.is_some()
        || !select.lateral_views.is_empty()
        || select.prewhere.is_some()
        || select.having.is_some()
        || select.qualify.is_some()
    {
        bail!("Only SELECT projection, FROM, WHERE, ORDER BY, and LIMIT are supported");
    }

    let columns = parse_projection(&select.projection)?;
    let sorts = parse_order_by(order_by)?;
    let limit = parse_limit(limit_clause)?;

    if matches!(columns, SelectColumns::Count) {
        if !sorts.is_empty() {
            bail!("ORDER BY is not supported with COUNT");
        }
        if limit.is_some() {
            bail!("LIMIT is not supported with COUNT");
        }
    }

    Ok(SqlStatement::Select {
        database: table_name_from_select_from(&select.from)?,
        columns,
        filter: select.selection,
        sorts,
        limit,
    })
}

/// Parses and validates an `INSERT INTO ... VALUES` statement.
///
/// Only the `INSERT INTO <table> (cols...) VALUES (...)` shape is accepted: an
/// explicit column list plus literal `VALUES` rows map directly to Notion page
/// creation without consulting any other relation.
///
/// # Parameters
///
/// - `insert`: the `sqlparser` insert AST taken from a `Statement::Insert`.
///
/// # Returns
///
/// A [`SqlStatement::Insert`].
///
/// # Errors
///
/// Returns an error for `INSERT ... SET`, `RETURNING`, inserts into table
/// functions, a source that is not `VALUES` (e.g. `INSERT ... SELECT`), a
/// missing source, or an omitted column list.
fn parse_insert(insert: sqlparser::ast::Insert) -> Result<SqlStatement> {
    if !insert.assignments.is_empty() {
        bail!("INSERT ... SET is not supported");
    }
    if insert.returning.is_some() {
        bail!("INSERT ... RETURNING is not supported");
    }

    let database = match insert.table {
        TableObject::TableName(name) => object_name_to_string(&name)?,
        TableObject::TableFunction(_) => bail!("INSERT into table functions is not supported"),
    };

    // Only VALUES rows can be converted into page creation payloads without
    // querying another source relation.
    let source = insert
        .source
        .ok_or_else(|| anyhow!("INSERT must use VALUES tuples"))?;
    let rows = match *source.body {
        SetExpr::Values(values) => values.rows,
        other => bail!("INSERT source must be VALUES, got '{other}'"),
    };

    if insert.columns.is_empty() {
        bail!("INSERT must specify target columns");
    }

    Ok(SqlStatement::Insert {
        database,
        columns: insert.columns.iter().map(ident_to_string).collect(),
        rows,
    })
}

/// Parses a `SELECT` projection into wildcard or explicit column form.
///
/// Recognizes three shapes in priority order: a lone `*` wildcard, a lone
/// `COUNT(*)`/`COUNT(1)`, then a list of bare column names.
///
/// # Parameters
///
/// - `projection`: the projection items from the parsed `SELECT`.
///
/// # Returns
///
/// The matching [`SelectColumns`] variant.
///
/// # Errors
///
/// Returns an error for aliased expressions, qualified wildcards
/// (`table.*`), a wildcard mixed with other items, or any projection item that
/// is not a plain column name (e.g. arithmetic or unsupported functions).
fn parse_projection(projection: &[SelectItem]) -> Result<SelectColumns> {
    // `SELECT *` is only valid as the sole projection item.
    if projection.len() == 1 && matches!(projection[0], SelectItem::Wildcard(_)) {
        return Ok(SelectColumns::All);
    }

    // A single unnamed expression might be a COUNT aggregate; probe before
    // falling through to the generic column-name handling below.
    if projection.len() == 1 {
        if let SelectItem::UnnamedExpr(expr) = &projection[0] {
            if is_count_projection(expr)? {
                return Ok(SelectColumns::Count);
            }
        }
    }

    let columns = projection
        .iter()
        .map(|item| match item {
            SelectItem::UnnamedExpr(expr) => column_expr_name(expr),
            SelectItem::ExprWithAlias { .. } => bail!("SELECT aliases are not supported"),
            SelectItem::QualifiedWildcard(_, _) => bail!("Qualified wildcards are not supported"),
            SelectItem::Wildcard(_) => bail!("Wildcard must be the only SELECT item"),
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(SelectColumns::Columns(columns))
}

/// Detects the supported `COUNT` aggregate projection shapes.
///
/// Distinguishes "this is not a count at all" (returns `Ok(false)`, so the
/// caller can treat the expression as an ordinary column) from "this looks like
/// a count but uses an unsupported form" (returns an error). Only bare
/// `COUNT(*)` and `COUNT(1)` are supported.
///
/// # Parameters
///
/// - `expr`: a single unnamed projection expression.
///
/// # Returns
///
/// `Ok(true)` for a supported count, `Ok(false)` for a non-count expression.
///
/// # Errors
///
/// Returns an error when the expression is a `COUNT` call but carries
/// unsupported decoration — `DISTINCT`, a `FILTER`, null treatment, an `OVER`
/// window, `WITHIN GROUP`, the wrong argument count, or an argument other than
/// `*` / `1`.
fn is_count_projection(expr: &Expr) -> Result<bool> {
    // Anything that is not a function call cannot be a COUNT aggregate.
    let Expr::Function(function) = expr else {
        return Ok(false);
    };

    // Function name match is case-insensitive per SQL identifier rules.
    if !function.name.to_string().eq_ignore_ascii_case("count") {
        return Ok(false);
    }

    if function.parameters != FunctionArguments::None
        || function.filter.is_some()
        || function.null_treatment.is_some()
        || function.over.is_some()
        || !function.within_group.is_empty()
    {
        bail!("Only plain COUNT(*) or COUNT(1) is supported");
    }

    let FunctionArguments::List(args) = &function.args else {
        bail!("COUNT requires one argument");
    };

    if args.duplicate_treatment.is_some() || !args.clauses.is_empty() || args.args.len() != 1 {
        bail!("Only plain COUNT(*) or COUNT(1) is supported");
    }

    match &args.args[0] {
        // COUNT(*)
        FunctionArg::Unnamed(FunctionArgExpr::Wildcard) => Ok(true),
        // COUNT(1): the numeric literal is matched on its textual form ("1")
        // rather than a parsed integer, because `sqlparser` stores numbers as
        // strings to preserve the original lexeme.
        FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(value))) if matches!(&value.value, Value::Number(number, _) if number == "1") => {
            Ok(true)
        }
        _ => bail!("Only COUNT(*) and COUNT(1) are supported"),
    }
}

/// Parses `ORDER BY` expressions into Notion sort specifications.
///
/// # Parameters
///
/// - `order_by`: the optional `ORDER BY` clause; `None` yields an empty vector.
///
/// # Returns
///
/// One [`SortSpec`] per ordering expression, in declared priority order.
///
/// # Errors
///
/// Returns an error for `ORDER BY ... INTERPOLATE`, for `ORDER BY ALL`, or when
/// an ordering expression is not a bare column name.
fn parse_order_by(order_by: Option<sqlparser::ast::OrderBy>) -> Result<Vec<SortSpec>> {
    let Some(order_by) = order_by else {
        return Ok(Vec::new());
    };

    if order_by.interpolate.is_some() {
        bail!("ORDER BY INTERPOLATE is not supported");
    }

    let OrderByKind::Expressions(exprs) = order_by.kind else {
        bail!("ORDER BY ALL is not supported");
    };

    exprs
        .iter()
        .map(|expr| {
            Ok(SortSpec {
                column: column_expr_name(&expr.expr)?,
                // `asc` is `None` when neither ASC nor DESC was written; SQL's
                // default direction is ascending.
                ascending: expr.options.asc.unwrap_or(true),
            })
        })
        .collect()
}

/// Parses a supported `LIMIT` clause into a non-negative integer.
///
/// # Parameters
///
/// - `limit_clause`: the optional `LIMIT` clause; `None` yields `Ok(None)`.
///
/// # Returns
///
/// `Ok(Some(n))` for a literal non-negative integer limit, `Ok(None)` when
/// there is no limit (or the clause has no count expression).
///
/// # Errors
///
/// Returns an error for `LIMIT ... OFFSET`, `LIMIT ... BY`, the MySQL
/// `LIMIT offset, count` form, or a limit value that is not a non-negative
/// whole number.
fn parse_limit(limit_clause: Option<LimitClause>) -> Result<Option<usize>> {
    let Some(limit_clause) = limit_clause else {
        return Ok(None);
    };

    let expr = match limit_clause {
        LimitClause::LimitOffset {
            limit,
            offset,
            limit_by,
        } => {
            if offset.is_some() || !limit_by.is_empty() {
                bail!("LIMIT OFFSET/BY is not supported");
            }
            limit
        }
        LimitClause::OffsetCommaLimit { .. } => bail!("MySQL LIMIT offset,count is not supported"),
    };

    let Some(expr) = expr else {
        return Ok(None);
    };

    // Limits arrive as a number literal; reuse the crate's literal evaluator and
    // then enforce that the value is a non-negative integer (no fractional part)
    // before narrowing the f64 to `usize`.
    match crate::value::literal_from_expr(&expr)? {
        crate::value::Literal::Number(value) if value >= 0.0 && value.fract() == 0.0 => {
            Ok(Some(value as usize))
        }
        other => bail!("LIMIT must be a non-negative integer, got {other:?}"),
    }
}

/// Extracts the only table name allowed in a `SELECT FROM` clause.
///
/// # Parameters
///
/// - `from`: the `FROM` list of a parsed `SELECT`.
///
/// # Returns
///
/// The single referenced database name/ID.
///
/// # Errors
///
/// Returns an error unless `from` contains exactly one table reference, or if
/// that reference is a join or non-table relation (delegated to
/// [`table_name_from_table_with_joins`]).
fn table_name_from_select_from(from: &[TableWithJoins]) -> Result<String> {
    if from.len() != 1 {
        bail!("SELECT must reference exactly one database");
    }
    table_name_from_table_with_joins(&from[0])
}

/// Extracts the only table name allowed in a `DELETE FROM` clause.
///
/// # Parameters
///
/// - `from`: the `DELETE` target, which may or may not use the `FROM` keyword.
///
/// # Returns
///
/// The single referenced database name/ID.
///
/// # Errors
///
/// Returns an error unless exactly one table is referenced, or if that
/// reference is a join or non-table relation.
fn table_name_from_from_table(from: &FromTable) -> Result<String> {
    // Both delete spellings (`DELETE FROM t` and the keyword-less `DELETE t`)
    // carry the same table list, so collapse them into one branch.
    let tables = match from {
        FromTable::WithFromKeyword(tables) | FromTable::WithoutKeyword(tables) => tables,
    };

    if tables.len() != 1 {
        bail!("DELETE must reference exactly one database");
    }
    table_name_from_table_with_joins(&tables[0])
}

/// Extracts a plain table name and rejects joins or non-table relations.
///
/// Shared by `SELECT`, `UPDATE`, and `DELETE` so the "single plain table only"
/// rule is enforced identically everywhere.
///
/// # Parameters
///
/// - `table`: a relation plus any joins from the parsed statement.
///
/// # Returns
///
/// The database name/ID of the relation.
///
/// # Errors
///
/// Returns an error if any join is present, or if the relation is anything
/// other than a plain table (subquery, derived table, table function, etc.).
fn table_name_from_table_with_joins(table: &TableWithJoins) -> Result<String> {
    if !table.joins.is_empty() {
        bail!("JOINs are not supported");
    }

    match &table.relation {
        TableFactor::Table { name, .. } => object_name_to_string(name),
        other => bail!("Expected a database name or ID, got '{other}'"),
    }
}

/// Converts an `UPDATE SET` target into a single column name.
///
/// # Parameters
///
/// - `target`: the left-hand side of one `SET` assignment.
///
/// # Returns
///
/// The assigned column's name.
///
/// # Errors
///
/// Returns an error for tuple targets (`SET (a, b) = ...`), which Notion's
/// per-property update model cannot express.
fn assignment_target_name(target: &AssignmentTarget) -> Result<String> {
    match target {
        AssignmentTarget::ColumnName(name) => object_name_to_string(name),
        AssignmentTarget::Tuple(_) => bail!("Tuple assignments are not supported"),
    }
}

/// Converts a projection or sort expression into a single column name.
///
/// Accepts a bare identifier or a one-part compound identifier; the latter
/// covers quoting/parsing quirks that still denote a single unqualified name.
///
/// # Parameters
///
/// - `expr`: a projection or `ORDER BY` expression expected to name a column.
///
/// # Returns
///
/// The column name.
///
/// # Errors
///
/// Returns an error for multi-part qualified names (`t.col`) and for any
/// expression that is not a plain column reference.
fn column_expr_name(expr: &Expr) -> Result<String> {
    match expr {
        Expr::Identifier(ident) => Ok(ident_to_string(ident)),
        Expr::CompoundIdentifier(parts) if parts.len() == 1 => Ok(ident_to_string(&parts[0])),
        Expr::CompoundIdentifier(_) => bail!("Qualified column names are not supported"),
        other => bail!("Expected a column name, got '{other}'"),
    }
}

/// Converts a possibly quoted object name into the string used for a database or column.
///
/// Multi-part names are rejoined with `.`; in practice a Notion database name or
/// ID is a single part, but this preserves any dotted name the user wrote so it
/// can be matched or reported faithfully.
///
/// # Parameters
///
/// - `name`: the parsed object name (one or more identifier parts).
///
/// # Returns
///
/// The dot-joined name string.
///
/// # Errors
///
/// Returns an error if any part is a function-style identifier, or if the name
/// has no identifier parts at all.
fn object_name_to_string(name: &ObjectName) -> Result<String> {
    let mut parts = Vec::new();
    for part in &name.0 {
        match part {
            ObjectNamePart::Identifier(ident) => parts.push(ident_to_string(ident)),
            ObjectNamePart::Function(_) => bail!("Function-style identifiers are not supported"),
        }
    }

    if parts.is_empty() {
        bail!("Expected a database name or ID");
    }

    Ok(parts.join("."))
}

/// Returns the identifier text exactly as parsed by `sqlparser`.
///
/// Returns the unquoted inner value, deliberately dropping any surrounding quote
/// character. This means `"Name"` and `Name` both yield `Name`, matching how
/// callers compare names against the Notion schema.
///
/// # Parameters
///
/// - `ident`: the parsed identifier token.
///
/// # Returns
///
/// An owned copy of the identifier's textual value.
fn ident_to_string(ident: &Ident) -> String {
    ident.value.clone()
}
