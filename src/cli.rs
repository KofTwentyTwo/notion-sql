//! Command line entrypoint and statement execution coordinator.
//!
//! This module owns the boundary between parsed SQL, Notion API operations, and
//! console output. Lower-level modules parse statements, translate filters,
//! coerce values, and render results; this module wires them together and
//! enforces the cross-cutting policies that do not belong to any single layer.
//!
//! # Responsibilities
//! - Define the CLI surface ([`CliArgs`]) and the extended SQL help text
//!   (`SQL_HELP`) presented by `--help`.
//! - Parse arguments, construct the [`NotionClient`], and dispatch the requested
//!   action ([`run`]).
//! - Execute a parsed [`SqlStatement`] against Notion (`execute`), translating
//!   friendly column names to canonical Notion property names only *after* the
//!   schema has been fetched.
//! - Enforce the destructive-mutation safety policy
//!   ([`guard_applied_full_table_mutation`]).
//! - Emit optional human-readable progress to stderr (`ProgressReporter`).
//!
//! # Key invariant: dry-run by default
//! INSERT, UPDATE, and DELETE always perform the read (query) path so the user
//! can preview the affected rows. The Notion write calls only happen when
//! `--apply` is passed. This keeps preview and apply behavior identical apart
//! from the final mutating call, so a dry-run faithfully predicts an apply.
//!
//! # Why translate filters/sorts after schema lookup
//! SQL identifiers in the user's statement are friendly column names. Notion's
//! query API requires canonical property names (and their types). Filters and
//! sorts therefore cannot be built until the database schema has been retrieved,
//! which is why every branch resolves the database, fetches the schema, and only
//! then translates `WHERE`/`ORDER BY`.

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
///
/// Derived from `clap`; field-level `#[arg(...)]` attributes define the public
/// CLI contract (flag names, mutual exclusions, and the "required unless
/// `--list-databases`" rule for the positional SQL argument). The doc comment on
/// each field is what `clap` surfaces as that flag's `--help` blurb, so the
/// wording is user-facing, not just internal.
#[derive(Debug, Parser)]
#[command(author, version, about, after_long_help = SQL_HELP)]
pub struct CliArgs {
    /// SQL statement to execute.
    ///
    /// Optional only because `--list-databases` is a standalone mode that needs
    /// no SQL; `required_unless_present` enforces that exactly one of the two is
    /// supplied so the binary always has something to do.
    #[arg(required_unless_present = "list_databases")]
    pub sql: Option<String>,

    /// List Notion databases visible to the integration token.
    #[arg(long)]
    pub list_databases: bool,

    /// Actually execute INSERT, UPDATE, and DELETE. Without this flag they run as dry-runs.
    #[arg(long)]
    pub apply: bool,

    /// Print SELECT results as JSON.
    ///
    /// Mutually exclusive with `--csv`; `clap` rejects passing both before this
    /// struct is ever constructed.
    #[arg(long, conflicts_with = "csv")]
    pub json: bool,

    /// Print SELECT results as CSV.
    ///
    /// Mutually exclusive with `--json` (see [`CliArgs::json`]).
    #[arg(long, conflicts_with = "json")]
    pub csv: bool,

    /// Show progress for long-running queries and mutations on stderr.
    ///
    /// Progress is written to stderr (not stdout) so it never contaminates the
    /// machine-readable result stream produced by `--json`/`--csv`.
    #[arg(long)]
    pub progress: bool,

    /// Allow applied UPDATE or DELETE statements without a WHERE clause.
    ///
    /// The escape hatch for the full-table safety guard; only consulted when
    /// `--apply` is also set (see [`guard_applied_full_table_mutation`]).
    #[arg(long)]
    pub force_all: bool,
}

/// Extended help text documenting the supported SQL surface.
///
/// Wired into `clap` via `after_long_help` so it appears only under
/// `--help` (the long form), keeping the short `-h` output terse. Kept as a
/// hand-maintained string because the supported SQL grammar is narrower than
/// what `sqlparser` accepts and must be described in user terms.
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
    ///
    /// `--json` and `--csv` are guaranteed mutually exclusive by `clap`, so the
    /// precedence here (json, then csv, then the table default) only matters for
    /// readability; both cannot be set at once. Returns [`OutputFormat::Table`]
    /// when neither flag is present.
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
///
/// This is the crate's top-level entrypoint, intended to be called from `main`.
/// It handles the `--list-databases` mode inline (it is a self-contained action
/// that needs no SQL) and otherwise parses the SQL statement and hands off to
/// `execute`.
///
/// # Errors
/// Returns an error if `NOTION_TOKEN` is unset, if the Notion client cannot be
/// constructed, if listing databases fails, if no SQL was supplied outside
/// `--list-databases` mode, if the SQL fails to parse, or if `execute`
/// propagates a failure.
pub fn run() -> Result<()> {
    let args = CliArgs::parse();
    // The integration token is read from the environment rather than a flag so
    // it never lands in shell history or process listings.
    let token = env::var("NOTION_TOKEN").context("NOTION_TOKEN is required")?;
    let mut client = NotionClient::new(token)?;

    // `--list-databases` is a standalone mode: short-circuit before requiring SQL.
    if args.list_databases {
        let databases = client.list_databases()?;
        println!("{}", render_databases(&databases, args.output_format())?);
        return Ok(());
    }

    // `clap`'s `required_unless_present` already guarantees `sql` is set here;
    // the `context` is defense-in-depth in case that invariant ever changes.
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
///
/// Dispatches on the statement variant. For every variant the database is
/// resolved and its schema fetched first, because filter/sort/value translation
/// all depend on canonical property names and types. Mutating variants (INSERT,
/// UPDATE, DELETE) always perform the read path so the affected rows can be
/// previewed; the actual Notion writes are gated behind `apply`.
///
/// # Parameters
/// - `statement`: the parsed SQL to run.
/// - `client`: the Notion client; taken by `&mut` because resolution and schema
///   lookups may populate internal caches.
/// - `apply`: when `true`, mutations are written to Notion; when `false`, they
///   are previewed only (dry-run).
/// - `force_all`: opt-in that lets an applied UPDATE/DELETE run without a
///   `WHERE` clause; see [`guard_applied_full_table_mutation`].
/// - `progress_enabled`: when `true`, progress lines are emitted to stderr.
/// - `output_format`: how SELECT/COUNT results are rendered.
///
/// # Errors
/// Returns an error if the full-table guard rejects the statement, if the
/// database cannot be resolved, if schema retrieval fails, if `WHERE`/`ORDER BY`
/// translation or value coercion fails, if the Notion query or any mutation call
/// fails, or if rendering the result fails.
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
                // COUNT(*) only needs the row tally, not the row contents.
                SelectColumns::Count => {
                    println!("{}", render_count(rows.len(), output_format)?);
                }
                // `*` or an explicit projection: resolve the column list, then render.
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

            // Notion has no bulk-delete API, so each matched page is trashed
            // individually. "Trash" (not hard-delete) mirrors Notion semantics:
            // pages move to the trash and remain recoverable.
            if apply {
                progress.mutation_started("trash", rows.len())?;
                for (index, row) in rows.iter().enumerate() {
                    progress.mutation_row("trashing", index + 1, rows.len(), row)?;
                    client.trash_page(&row.id)?;
                }
                progress.mutation_finished("trashed", rows.len())?;
            }
            // Always print the plan, whether applied or dry-run, so the user sees
            // exactly which rows were (or would be) affected.
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
            // Build the shared property payload once; it is identical for every
            // matched row and is cloned per page below.
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
            // INSERT has no `WHERE`, so there is no full-table guard here; the
            // user explicitly enumerated the rows to create.
            let payloads = build_insert_payloads(&columns, &rows, &schema)?;

            // One create_page call per VALUES row; Notion has no bulk insert.
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
///
/// Guards against the classic "forgot the `WHERE`" footgun: an applied UPDATE or
/// DELETE that would touch every row in a database. The guard only fires for
/// *applied* mutations (`apply == true`); dry-runs are always allowed because
/// they make no changes, and a filtered statement is always allowed because it
/// is scoped. Made `pub` so it can be exercised directly in tests.
///
/// # Parameters
/// - `statement`: the SQL verb (e.g. `"UPDATE"`/`"DELETE"`), used only in the
///   error message.
/// - `apply`: whether the statement would actually write to Notion.
/// - `force_all`: the user's explicit opt-in to affect every row.
/// - `has_filter`: whether the statement carried a `WHERE` clause.
///
/// # Errors
/// Returns an error when `apply` is set, `force_all` is not, and there is no
/// `WHERE` clause — i.e. an applied, unscoped, non-forced mutation.
pub fn guard_applied_full_table_mutation(
    statement: &str,
    apply: bool,
    force_all: bool,
    has_filter: bool,
) -> Result<()> {
    // Only an applied, unfiltered, non-forced mutation is dangerous; every other
    // combination is either harmless (dry-run), scoped (has filter), or
    // intentional (force_all).
    if apply && !force_all && !has_filter {
        bail!(
            "{statement} with --apply requires a WHERE clause. Add --force-all only if you intend to affect every row."
        );
    }

    Ok(())
}

/// Optional stderr progress renderer for slow Notion operations.
///
/// A thin wrapper whose only state is the enabled flag. Every reporting method
/// is a no-op when disabled, so callers can sprinkle progress calls
/// unconditionally without branching on whether `--progress` was passed. All
/// output goes to stderr to keep stdout clean for results.
struct ProgressReporter {
    /// Whether progress output is enabled for this run.
    ///
    /// When `false`, every reporting method returns `Ok(())` without writing.
    enabled: bool,
}

impl ProgressReporter {
    /// Creates a progress reporter that either emits to stderr or stays silent.
    ///
    /// `enabled` is typically the value of the `--progress` flag.
    fn new(enabled: bool) -> Self {
        Self { enabled }
    }

    /// Reports the start of a database query.
    ///
    /// `database_id` is the resolved Notion ID; `limit` is the row cap, where
    /// `None` means "all matching rows" and changes the wording accordingly.
    ///
    /// # Errors
    /// Returns an error if writing to stderr fails (only when enabled).
    fn query_started(&mut self, database_id: &str, limit: Option<usize>) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }

        // Distinct wording for capped vs. uncapped queries so the user knows
        // whether a LIMIT is in effect.
        match limit {
            Some(limit) => self.line(&format!("querying {database_id}, limit {limit} rows")),
            None => self.line(&format!("querying {database_id}, all matching rows")),
        }
    }

    /// Reports the final number of rows fetched by a query.
    ///
    /// `rows` is the total matched count.
    ///
    /// # Errors
    /// Returns an error if writing to stderr fails (only when enabled).
    fn query_finished(&mut self, rows: usize) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }

        self.line(&format!("query complete, {rows} rows matched"))
    }

    /// Reports one completed Notion query page fetch.
    ///
    /// Invoked once per paginated API response. `pages` is the running page
    /// count and `rows` the running matched-row count.
    ///
    /// # Errors
    /// Returns an error if writing to stderr fails (only when enabled).
    fn query_page(&mut self, pages: usize, rows: usize) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }

        self.line(&format!("fetched {pages} query pages, {rows} rows matched"))
    }

    /// Reports the start of a row-by-row mutation.
    ///
    /// `verb` is the present-tense action (e.g. `"update"`, `"trash"`) and
    /// `total` the number of rows about to be mutated.
    ///
    /// # Errors
    /// Returns an error if writing to stderr fails (only when enabled).
    fn mutation_started(&mut self, verb: &str, total: usize) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }

        self.line(&format!("starting {verb} for {total} rows"))
    }

    /// Reports progress for one page mutation.
    ///
    /// Emits a `current/total` line identifying the affected page by title and
    /// ID so the user can correlate progress with specific rows.
    ///
    /// `verb` is the gerund action (e.g. `"updating"`); `current` is the 1-based
    /// position; `total` is the row count; `row` is the page being mutated.
    ///
    /// # Errors
    /// Returns an error if writing to stderr fails (only when enabled).
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
    ///
    /// Separate from [`ProgressReporter::mutation_row`] because inserted rows
    /// have no existing page identity (title/ID) to display yet; only the
    /// `current`/`total` counters are known.
    ///
    /// # Errors
    /// Returns an error if writing to stderr fails (only when enabled).
    fn insert_row(&mut self, current: usize, total: usize) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }

        self.line(&format!("inserting {current}/{total}"))
    }

    /// Reports the end of a row-by-row mutation.
    ///
    /// `verb` is the past-tense action (e.g. `"updated"`); `total` is the count.
    /// Renders as `total/total` to signal completion.
    ///
    /// # Errors
    /// Returns an error if writing to stderr fails (only when enabled).
    fn mutation_finished(&mut self, verb: &str, total: usize) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }

        self.line(&format!("{verb} {total}/{total} rows"))
    }

    /// Writes one progress line to stderr and flushes immediately.
    ///
    /// The shared sink for every reporting method. It locks stderr for the
    /// single write and flushes right away so progress appears in real time
    /// rather than being buffered until the program exits. The `[progress]`
    /// prefix makes these lines easy to distinguish (and filter out).
    ///
    /// # Errors
    /// Returns an error if the write or flush to stderr fails.
    fn line(&mut self, message: &str) -> Result<()> {
        let mut stderr = io::stderr().lock();
        writeln!(stderr, "[progress] {message}")?;
        // Flush eagerly: stderr is line-buffered or block-buffered depending on
        // the target, and we want progress visible while the work is ongoing.
        stderr.flush()?;
        Ok(())
    }
}

/// Resolves `SELECT *` or explicit projection items into canonical Notion property names.
///
/// Returns the ordered list of canonical property names to render.
///
/// # Behavior by variant
/// - [`SelectColumns::All`] (`*`): expands to every schema property, but only if
///   none of them are of an unsupported Notion type. Unsupported types are
///   rejected here (rather than silently dropped) so `*` never produces a
///   misleadingly partial result; the user must list supported columns instead.
/// - [`SelectColumns::Columns`]: resolves each user-supplied name to its
///   canonical property name.
/// - [`SelectColumns::Count`]: returns the single synthetic `"count"` column.
///   Reachable defensively even though the COUNT path in `execute` renders via
///   `render_count` and does not call this function.
///
/// # Errors
/// Returns an error if `SELECT *` encounters unsupported property types, or if
/// any explicitly named column cannot be resolved against the schema.
fn selected_columns(columns: &SelectColumns, schema: &DatabaseSchema) -> Result<Vec<String>> {
    match columns {
        SelectColumns::All => {
            // `*` must be all-or-nothing: refuse rather than quietly omit columns
            // whose Notion type the renderer cannot represent.
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
        // Explicit columns are resolved through the schema so friendly names map
        // to Notion's canonical property names; an unknown name is an error.
        SelectColumns::Columns(columns) => columns
            .iter()
            .map(|column| Ok(schema.resolve_property(column)?.name.clone()))
            .collect(),
        SelectColumns::Count => Ok(vec!["count".to_string()]),
    }
}

/// Builds Notion sort objects from parsed `ORDER BY` items.
///
/// Maps each [`SortSpec`] to the `{ "property", "direction" }` JSON shape the
/// Notion query API expects, resolving the column name and translating the
/// `ascending` boolean to Notion's string direction. Preserves the original sort
/// order so multi-column `ORDER BY` precedence is honored.
///
/// # Errors
/// Returns an error if any sort column cannot be resolved against the schema.
fn build_sorts(sorts: &[SortSpec], schema: &DatabaseSchema) -> Result<Vec<Value>> {
    sorts
        .iter()
        .map(|sort| {
            let property = schema.resolve_property(&sort.column)?;
            // Notion expects "ascending"/"descending" strings, not a boolean.
            Ok(json!({
                "property": property.name,
                "direction": if sort.ascending { "ascending" } else { "descending" }
            }))
        })
        .collect()
}

/// Converts `UPDATE SET` assignments into a Notion `properties` payload.
///
/// Resolves each assignment's column against the schema and coerces its value to
/// the property's Notion type, yielding the `properties` object expected by the
/// page-update API. A [`BTreeMap`] is used so the resulting JSON has a stable,
/// deterministic key order (helpful for reproducible dry-run plans and tests).
///
/// # Errors
/// Returns an error if any column cannot be resolved or if a value cannot be
/// coerced to its property's type.
fn build_assignment_payload(
    assignments: &[crate::sql::Assignment],
    schema: &DatabaseSchema,
) -> Result<Value> {
    let mut properties = BTreeMap::new();
    for assignment in assignments {
        let property = schema.resolve_property(&assignment.column)?;
        // Coerce the literal to the property's Notion type before inserting; a
        // later duplicate column would overwrite an earlier one in the map.
        properties.insert(
            property.name.clone(),
            coerce_property_value(property, &assignment.value)?,
        );
    }
    Ok(json!(properties))
}

/// Converts all `INSERT ... VALUES` rows into per-page Notion property payloads.
///
/// Produces one `properties` JSON object per VALUES row, each created by zipping
/// the shared `columns` list with that row's expressions, resolving each column,
/// and coercing each value. As in [`build_assignment_payload`], a [`BTreeMap`]
/// gives deterministic key ordering.
///
/// # Parameters
/// - `columns`: the target column names, shared across all rows.
/// - `rows`: the per-row value expressions parsed by `sqlparser`.
/// - `schema`: the database schema used to resolve columns and coerce values.
///
/// # Errors
/// Returns an error if any row's value count does not match the column count, if
/// a column cannot be resolved, or if a value cannot be coerced to its type.
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

            // Pair each column with its positional value; `zip` is safe because
            // the length check above guarantees equal lengths.
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
