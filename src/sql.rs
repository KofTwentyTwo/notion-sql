//! SQL parsing and validation for the subset supported by `notion-sql`.
//!
//! `sqlparser` accepts a broad SQL grammar. This module constrains that AST to
//! simple single-table CRUD operations that can be translated to Notion.

use anyhow::{anyhow, bail, Context, Result};
use sqlparser::ast::{
    AssignmentTarget, Expr, FromTable, FunctionArg, FunctionArgExpr, FunctionArguments, Ident,
    LimitClause, ObjectName, ObjectNamePart, OrderByKind, Query, SelectItem, SetExpr, Statement,
    TableFactor, TableObject, TableWithJoins, Value,
};
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;

/// Projection requested by a `SELECT` statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectColumns {
    /// `SELECT *`, resolved later through the Notion schema.
    All,
    /// Explicit column names in SQL projection order.
    Columns(Vec<String>),
    /// `SELECT COUNT(*)` or `SELECT COUNT(1)`, rendered as a one-row aggregate.
    Count,
}

/// One parsed `ORDER BY` item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SortSpec {
    /// Column to sort by, resolved later through the Notion schema.
    pub column: String,
    /// `true` for ascending order and `false` for descending order.
    pub ascending: bool,
}

/// One parsed `UPDATE SET` assignment.
#[derive(Debug, Clone, PartialEq)]
pub struct Assignment {
    /// Column receiving the assignment.
    pub column: String,
    /// SQL expression for the assigned literal value.
    pub value: Expr,
}

/// Supported SQL statement shapes after validation.
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
        sorts: Vec<SortSpec>,
        /// Optional row limit.
        limit: Option<usize>,
    },
    /// An `INSERT INTO ... VALUES` statement.
    Insert {
        /// Database name or ID from the target table.
        database: String,
        /// Target columns receiving values.
        columns: Vec<String>,
        /// Values grouped by row.
        rows: Vec<Vec<Expr>>,
    },
    /// An `UPDATE` statement.
    Update {
        /// Database name or ID from the update target.
        database: String,
        /// Property assignments to apply to matched pages.
        assignments: Vec<Assignment>,
        /// Optional `WHERE` expression.
        filter: Option<Expr>,
    },
    /// A `DELETE` statement, implemented as moving pages to trash.
    Delete {
        /// Database name or ID from the delete target.
        database: String,
        /// Optional `WHERE` expression.
        filter: Option<Expr>,
    },
}

/// Parses one SQL string into a supported statement representation.
pub fn parse_statement(sql: &str) -> Result<SqlStatement> {
    let dialect = GenericDialect {};
    let statements = Parser::parse_sql(&dialect, sql).context("Failed to parse SQL")?;
    if statements.len() != 1 {
        bail!("Expected exactly one SQL statement");
    }

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
fn parse_projection(projection: &[SelectItem]) -> Result<SelectColumns> {
    if projection.len() == 1 && matches!(projection[0], SelectItem::Wildcard(_)) {
        return Ok(SelectColumns::All);
    }

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
fn is_count_projection(expr: &Expr) -> Result<bool> {
    let Expr::Function(function) = expr else {
        return Ok(false);
    };

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
        FunctionArg::Unnamed(FunctionArgExpr::Wildcard) => Ok(true),
        FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(value))) if matches!(&value.value, Value::Number(number, _) if number == "1") => {
            Ok(true)
        }
        _ => bail!("Only COUNT(*) and COUNT(1) are supported"),
    }
}

/// Parses `ORDER BY` expressions into Notion sort specifications.
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
                ascending: expr.options.asc.unwrap_or(true),
            })
        })
        .collect()
}

/// Parses a supported `LIMIT` clause into a non-negative integer.
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

    match crate::value::literal_from_expr(&expr)? {
        crate::value::Literal::Number(value) if value >= 0.0 && value.fract() == 0.0 => {
            Ok(Some(value as usize))
        }
        other => bail!("LIMIT must be a non-negative integer, got {other:?}"),
    }
}

/// Extracts the only table name allowed in a `SELECT FROM` clause.
fn table_name_from_select_from(from: &[TableWithJoins]) -> Result<String> {
    if from.len() != 1 {
        bail!("SELECT must reference exactly one database");
    }
    table_name_from_table_with_joins(&from[0])
}

/// Extracts the only table name allowed in a `DELETE FROM` clause.
fn table_name_from_from_table(from: &FromTable) -> Result<String> {
    let tables = match from {
        FromTable::WithFromKeyword(tables) | FromTable::WithoutKeyword(tables) => tables,
    };

    if tables.len() != 1 {
        bail!("DELETE must reference exactly one database");
    }
    table_name_from_table_with_joins(&tables[0])
}

/// Extracts a plain table name and rejects joins or non-table relations.
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
fn assignment_target_name(target: &AssignmentTarget) -> Result<String> {
    match target {
        AssignmentTarget::ColumnName(name) => object_name_to_string(name),
        AssignmentTarget::Tuple(_) => bail!("Tuple assignments are not supported"),
    }
}

/// Converts a projection or sort expression into a single column name.
fn column_expr_name(expr: &Expr) -> Result<String> {
    match expr {
        Expr::Identifier(ident) => Ok(ident_to_string(ident)),
        Expr::CompoundIdentifier(parts) if parts.len() == 1 => Ok(ident_to_string(&parts[0])),
        Expr::CompoundIdentifier(_) => bail!("Qualified column names are not supported"),
        other => bail!("Expected a column name, got '{other}'"),
    }
}

/// Converts a possibly quoted object name into the string used for a database or column.
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
fn ident_to_string(ident: &Ident) -> String {
    ident.value.clone()
}
