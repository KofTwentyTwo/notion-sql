//! Command line entrypoint and statement execution coordinator.
//!
//! This module owns the boundary between parsed SQL, Notion API operations, and
//! console output. Lower-level modules parse statements, translate filters,
//! coerce values, and render results.

use std::collections::BTreeMap;
use std::env;
use std::io::{self, Write};

use anyhow::{bail, Context, Result};
use clap::Parser;
use serde_json::{json, Value};

use crate::filter::translate_where;
use crate::notion::NotionClient;
use crate::output::{
    print_delete_plan, print_insert_plan, print_update_plan, render_count, render_databases,
    render_select, OutputFormat,
};
use crate::schema::DatabaseSchema;
use crate::sql::{parse_statement, SelectColumns, SortSpec, SqlStatement};
use crate::value::coerce_property_value;

/// Parsed command line flags for the `notion-sql` binary.
#[derive(Debug, Parser)]
#[command(author, version, about, after_long_help = SQL_HELP)]
pub struct CliArgs {
    /// SQL statement to execute.
    #[arg(required_unless_present = "list_databases")]
    pub sql: Option<String>,

    /// List Notion databases visible to the integration token.
    #[arg(long)]
    pub list_databases: bool,

    /// Actually execute INSERT, UPDATE, and DELETE. Without this flag they run as dry-runs.
    #[arg(long)]
    pub apply: bool,

    /// Print SELECT results as JSON.
    #[arg(long, conflicts_with = "csv")]
    pub json: bool,

    /// Print SELECT results as CSV.
    #[arg(long, conflicts_with = "json")]
    pub csv: bool,

    /// Show progress for long-running queries and mutations on stderr.
    #[arg(long)]
    pub progress: bool,

    /// Allow applied UPDATE or DELETE statements without a WHERE clause.
    #[arg(long)]
    pub force_all: bool,
}

/// Extended help text documenting the supported SQL surface.
const SQL_HELP: &str = r#"SQL:
  SELECT <cols|*> FROM <db> [WHERE ...] [ORDER BY col [ASC|DESC]] [LIMIT n]
  SELECT COUNT(*) FROM <db> [WHERE ...]
  INSERT INTO <db> (col1, col2) VALUES (v1, v2), ...
  UPDATE <db> SET col=val[, col2=val2 ...] [WHERE ...]
  DELETE FROM <db> [WHERE ...]

Database references:
  Use a Notion database ID or exact database name.
  Quote database names with spaces:
    notion-sql "SELECT * FROM \"Task List\" LIMIT 5"

Examples:
  notion-sql --list-databases
  notion-sql "SELECT Name, Status FROM \"Task List\" WHERE Status='Done'"
  notion-sql "SELECT COUNT(*) FROM \"Task List\" WHERE Status='Done'"
  notion-sql "INSERT INTO \"Task List\" (Name, Status) VALUES ('New task', 'To Do')"
  notion-sql "UPDATE \"Task List\" SET Status='Archived' WHERE Priority='Low'" --apply
  notion-sql "DELETE FROM \"Task List\" WHERE Status='Done'" --apply

WHERE support:
  =, !=, >, <, >=, <=, LIKE, IN (...), IS NULL, IS NOT NULL
  AND/OR grouping with parentheses is supported.

Output:
  SELECT, COUNT, and --list-databases default to a table.
  Add --json or --csv for machine-readable output.

Safety:
  INSERT, UPDATE, and DELETE are dry-run by default.
  Add --apply to write changes to Notion.
  UPDATE and DELETE require WHERE with --apply unless --force-all is also passed.
  Add --progress to show long-running query and mutation progress on stderr."#;

impl CliArgs {
    /// Converts the mutually exclusive format flags into the renderer enum.
    fn output_format(&self) -> OutputFormat {
        if self.json {
            OutputFormat::Json
        } else if self.csv {
            OutputFormat::Csv
        } else {
            OutputFormat::Table
        }
    }
}

/// Parses CLI arguments, builds the Notion client, and dispatches the requested action.
pub fn run() -> Result<()> {
    let args = CliArgs::parse();
    let token = env::var("NOTION_TOKEN").context("NOTION_TOKEN is required")?;
    let mut client = NotionClient::new(token)?;

    if args.list_databases {
        let databases = client.list_databases()?;
        println!("{}", render_databases(&databases, args.output_format())?);
        return Ok(());
    }

    let sql = args
        .sql
        .as_deref()
        .context("SQL statement is required unless --list-databases is used")?;
    let statement = parse_statement(sql)?;

    execute(
        statement,
        &mut client,
        args.apply,
        args.force_all,
        args.progress,
        args.output_format(),
    )
}

/// Executes a parsed SQL statement against Notion, preserving dry-run behavior for mutations.
fn execute(
    statement: SqlStatement,
    client: &mut NotionClient,
    apply: bool,
    force_all: bool,
    progress_enabled: bool,
    output_format: OutputFormat,
) -> Result<()> {
    let mut progress = ProgressReporter::new(progress_enabled);

    match statement {
        SqlStatement::Select {
            database,
            columns,
            filter,
            sorts,
            limit,
        } => {
            let database_id = client.resolve_database(&database)?;
            let schema = client.retrieve_schema(&database_id)?;
            // Filters and sorts must be built after schema lookup so friendly
            // column names are resolved to Notion's canonical property names.
            let filter = filter
                .as_ref()
                .map(|expr| translate_where(expr, &schema))
                .transpose()?;
            let sorts = build_sorts(&sorts, &schema)?;
            progress.query_started(&database_id, limit)?;
            let rows = client.query_database_with_progress(
                &database_id,
                filter,
                sorts,
                limit,
                |pages, rows| progress.query_page(pages, rows),
            )?;
            progress.query_finished(rows.len())?;
            match columns {
                SelectColumns::Count => {
                    println!("{}", render_count(rows.len(), output_format)?);
                }
                columns => {
                    let selected_columns = selected_columns(&columns, &schema)?;
                    println!(
                        "{}",
                        render_select(&rows, &selected_columns, output_format)?
                    );
                }
            }
        }
        SqlStatement::Delete { database, filter } => {
            guard_applied_full_table_mutation("DELETE", apply, force_all, filter.is_some())?;
            let database_id = client.resolve_database(&database)?;
            let schema = client.retrieve_schema(&database_id)?;
            let filter = filter
                .as_ref()
                .map(|expr| translate_where(expr, &schema))
                .transpose()?;
            progress.query_started(&database_id, None)?;
            let rows = client.query_database_with_progress(
                &database_id,
                filter,
                Vec::new(),
                None,
                |pages, rows| progress.query_page(pages, rows),
            )?;
            progress.query_finished(rows.len())?;

            if apply {
                progress.mutation_started("trash", rows.len())?;
                for (index, row) in rows.iter().enumerate() {
                    progress.mutation_row("trashing", index + 1, rows.len(), row)?;
                    client.trash_page(&row.id)?;
                }
                progress.mutation_finished("trashed", rows.len())?;
            }
            print_delete_plan(&rows, apply);
        }
        SqlStatement::Update {
            database,
            assignments,
            filter,
        } => {
            guard_applied_full_table_mutation("UPDATE", apply, force_all, filter.is_some())?;
            let database_id = client.resolve_database(&database)?;
            let schema = client.retrieve_schema(&database_id)?;
            let filter = filter
                .as_ref()
                .map(|expr| translate_where(expr, &schema))
                .transpose()?;
            let properties = build_assignment_payload(&assignments, &schema)?;
            progress.query_started(&database_id, None)?;
            let rows = client.query_database_with_progress(
                &database_id,
                filter,
                Vec::new(),
                None,
                |pages, rows| progress.query_page(pages, rows),
            )?;
            progress.query_finished(rows.len())?;

            // Mutating statements intentionally share the read path with dry-runs
            // so users can inspect the affected rows before adding `--apply`.
            if apply {
                progress.mutation_started("update", rows.len())?;
                for (index, row) in rows.iter().enumerate() {
                    progress.mutation_row("updating", index + 1, rows.len(), row)?;
                    client.update_page_properties(&row.id, properties.clone())?;
                }
                progress.mutation_finished("updated", rows.len())?;
            }
            print_update_plan(&rows, &properties, apply)?;
        }
        SqlStatement::Insert {
            database,
            columns,
            rows,
        } => {
            let database_id = client.resolve_database(&database)?;
            let schema = client.retrieve_schema(&database_id)?;
            let payloads = build_insert_payloads(&columns, &rows, &schema)?;

            if apply {
                progress.mutation_started("insert", payloads.len())?;
                for (index, payload) in payloads.iter().enumerate() {
                    progress.insert_row(index + 1, payloads.len())?;
                    client.create_page(&database_id, payload.clone())?;
                }
                progress.mutation_finished("inserted", payloads.len())?;
            }
            print_insert_plan(&payloads, apply)?;
        }
    }

    Ok(())
}

/// Rejects applied full-table mutations unless the user explicitly opts in.
fn guard_applied_full_table_mutation(
    statement: &str,
    apply: bool,
    force_all: bool,
    has_filter: bool,
) -> Result<()> {
    if apply && !force_all && !has_filter {
        bail!(
            "{statement} with --apply requires a WHERE clause. Add --force-all only if you intend to affect every row."
        );
    }

    Ok(())
}

/// Optional stderr progress renderer for slow Notion operations.
struct ProgressReporter {
    /// Whether progress output is enabled for this run.
    enabled: bool,
}

impl ProgressReporter {
    /// Creates a progress reporter that either emits to stderr or stays silent.
    fn new(enabled: bool) -> Self {
        Self { enabled }
    }

    /// Reports the start of a database query.
    fn query_started(&mut self, database_id: &str, limit: Option<usize>) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }

        match limit {
            Some(limit) => self.line(&format!("querying {database_id}, limit {limit} rows")),
            None => self.line(&format!("querying {database_id}, all matching rows")),
        }
    }

    /// Reports the final number of rows fetched by a query.
    fn query_finished(&mut self, rows: usize) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }

        self.line(&format!("query complete, {rows} rows matched"))
    }

    /// Reports one completed Notion query page fetch.
    fn query_page(&mut self, pages: usize, rows: usize) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }

        self.line(&format!("fetched {pages} query pages, {rows} rows matched"))
    }

    /// Reports the start of a row-by-row mutation.
    fn mutation_started(&mut self, verb: &str, total: usize) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }

        self.line(&format!("starting {verb} for {total} rows"))
    }

    /// Reports progress for one page mutation.
    fn mutation_row(
        &mut self,
        verb: &str,
        current: usize,
        total: usize,
        row: &crate::notion::PageRow,
    ) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }

        self.line(&format!(
            "{verb} {current}/{total}: {} ({})",
            row.title, row.id
        ))
    }

    /// Reports progress for one inserted row.
    fn insert_row(&mut self, current: usize, total: usize) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }

        self.line(&format!("inserting {current}/{total}"))
    }

    /// Reports the end of a row-by-row mutation.
    fn mutation_finished(&mut self, verb: &str, total: usize) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }

        self.line(&format!("{verb} {total}/{total} rows"))
    }

    /// Writes one progress line to stderr and flushes immediately.
    fn line(&mut self, message: &str) -> Result<()> {
        let mut stderr = io::stderr().lock();
        writeln!(stderr, "[progress] {message}")?;
        stderr.flush()?;
        Ok(())
    }
}

/// Resolves `SELECT *` or explicit projection items into canonical Notion property names.
fn selected_columns(columns: &SelectColumns, schema: &DatabaseSchema) -> Result<Vec<String>> {
    match columns {
        SelectColumns::All => {
            let unsupported = schema.unsupported_columns();
            if !unsupported.is_empty() {
                bail!(
                    "SELECT * includes unsupported Notion property types: {}. Select supported columns explicitly.",
                    unsupported.join(", ")
                );
            }
            Ok(schema
                .properties()
                .map(|property| property.name.clone())
                .collect())
        }
        SelectColumns::Columns(columns) => columns
            .iter()
            .map(|column| Ok(schema.resolve_property(column)?.name.clone()))
            .collect(),
        SelectColumns::Count => Ok(vec!["count".to_string()]),
    }
}

/// Builds Notion sort objects from parsed `ORDER BY` items.
fn build_sorts(sorts: &[SortSpec], schema: &DatabaseSchema) -> Result<Vec<Value>> {
    sorts
        .iter()
        .map(|sort| {
            let property = schema.resolve_property(&sort.column)?;
            Ok(json!({
                "property": property.name,
                "direction": if sort.ascending { "ascending" } else { "descending" }
            }))
        })
        .collect()
}

/// Converts `UPDATE SET` assignments into a Notion `properties` payload.
fn build_assignment_payload(
    assignments: &[crate::sql::Assignment],
    schema: &DatabaseSchema,
) -> Result<Value> {
    let mut properties = BTreeMap::new();
    for assignment in assignments {
        let property = schema.resolve_property(&assignment.column)?;
        properties.insert(
            property.name.clone(),
            coerce_property_value(property, &assignment.value)?,
        );
    }
    Ok(json!(properties))
}

/// Converts all `INSERT ... VALUES` rows into per-page Notion property payloads.
fn build_insert_payloads(
    columns: &[String],
    rows: &[Vec<sqlparser::ast::Expr>],
    schema: &DatabaseSchema,
) -> Result<Vec<Value>> {
    rows.iter()
        .enumerate()
        .map(|(row_index, row)| {
            if row.len() != columns.len() {
                // `sqlparser` accepts ragged VALUES lists, but Notion needs one
                // value per target column when creating page properties.
                bail!(
                    "INSERT row {} has {} values but {} columns were specified",
                    row_index + 1,
                    row.len(),
                    columns.len()
                );
            }

            let mut properties = BTreeMap::new();
            for (column, expr) in columns.iter().zip(row) {
                let property = schema.resolve_property(column)?;
                properties.insert(
                    property.name.clone(),
                    coerce_property_value(property, expr)?,
                );
            }
            Ok(json!(properties))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    //! Tests for CLI-level safety checks that do not require Notion HTTP access.

    use super::*;

    /// Verifies applied full-table updates and deletes require an explicit override.
    #[test]
    fn rejects_applied_full_table_mutation_without_force_all() {
        let error = guard_applied_full_table_mutation("DELETE", true, false, false).unwrap_err();

        assert!(error.to_string().contains("requires a WHERE clause"));
    }

    /// Verifies dry-runs and explicit full-table overrides remain available.
    #[test]
    fn allows_dry_run_or_forced_full_table_mutation() {
        guard_applied_full_table_mutation("DELETE", false, false, false).unwrap();
        guard_applied_full_table_mutation("DELETE", true, true, false).unwrap();
        guard_applied_full_table_mutation("DELETE", true, false, true).unwrap();
    }

    /// Verifies wildcard selection rejects unsupported Notion property types.
    #[test]
    fn rejects_select_all_with_unsupported_columns() {
        let schema = DatabaseSchema::from_notion_database(&json!({
            "properties": {
                "Name": { "type": "title", "title": {} },
                "Formula": { "type": "formula", "formula": {} }
            }
        }))
        .unwrap();

        let error = selected_columns(&SelectColumns::All, &schema).unwrap_err();

        assert!(error.to_string().contains("SELECT * includes unsupported"));
        assert!(error.to_string().contains("Formula (formula)"));
    }
}
