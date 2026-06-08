// SPDX-License-Identifier: MIT
// Copyright (c) 2026 James Maes
//! Output rendering for query results, database listings, and mutation plans.
//!
//! This module is the presentation layer of the crate. It takes the structured
//! domain types produced by the `notion` module ([`PageRow`], [`DatabaseInfo`])
//! and turns them into human- or machine-readable text in one of three formats
//! selected by [`OutputFormat`]: a UTF-8 terminal table, pretty JSON, or CSV.
//!
//! # Design choices
//!
//! - Renderers return `String` (via `Result<String>`) rather than writing to
//!   stdout directly. This keeps them pure and testable: callers and unit tests
//!   can inspect the exact bytes before anything is printed. The only functions
//!   that touch stdout are the `print_*_plan` helpers, which exist specifically
//!   to emit the dry-run / applied summaries of mutating commands where the
//!   interleaved structure (a header line, then a per-row list, then a JSON
//!   payload) is awkward to assemble into a single returned string.
//! - Every format is driven off the same property-extraction logic
//!   ([`property_string`]) so the table, JSON, and CSV views of the same data
//!   stay consistent — a value never renders one way in CSV and another in JSON.
//!
//! # Key items
//!
//! - [`OutputFormat`] — the three-way format selector threaded through the API.
//! - [`render_select`] / [`render_databases`] / [`render_count`] — the public
//!   entry points that dispatch on format and return the rendered text.
//! - [`print_delete_plan`] / [`print_update_plan`] / [`print_insert_plan`] —
//!   stdout-printing summaries for the mutating commands.
//! - [`property_string`] — the shared Notion-property-to-display-string mapper,
//!   public because the SQL layer also needs it for comparisons/sorting.

use anyhow::Result;
use comfy_table::{presets::UTF8_FULL, Cell, Table};
use serde_json::{json, Map, Value};

use crate::notion::{DatabaseInfo, PageRow};

/// Supported output formats for list, select, and count operations.
///
/// This is the single knob the CLI threads through the rendering layer to pick a
/// presentation. It is `Copy` so it can be passed by value freely; the variants
/// are mutually exclusive (exactly one format is rendered per invocation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    /// Render a UTF-8 box-drawing table for interactive terminal use.
    Table,
    /// Render pretty-printed JSON for scripting and machine consumption.
    Json,
    /// Render CSV for spreadsheet import and shell pipelines.
    Csv,
}

/// Renders selected page rows using the requested output format.
///
/// This is the public entry point for `SELECT`-style queries; it dispatches to
/// the per-format renderer.
///
/// # Parameters
/// - `rows`: the page rows to render, in the order they should appear.
/// - `columns`: the selected column names, in display order. These drive both
///   the header and which property is pulled from each row.
/// - `format`: which of the three presentations to produce.
///
/// # Returns
/// The fully rendered output as a single string.
///
/// # Errors
/// Returns an error if JSON serialization fails ([`OutputFormat::Json`]) or if
/// CSV writing / UTF-8 conversion fails ([`OutputFormat::Csv`]). The table path
/// is infallible but is wrapped in `Ok` to keep one uniform signature.
pub fn render_select(rows: &[PageRow], columns: &[String], format: OutputFormat) -> Result<String> {
    match format {
        OutputFormat::Table => Ok(render_table(rows, columns)),
        OutputFormat::Json => render_json(rows, columns),
        OutputFormat::Csv => render_csv(rows, columns),
    }
}

/// Renders database metadata using the requested output format.
///
/// Public entry point for the "list databases" command.
///
/// # Parameters
/// - `databases`: the visible databases to render.
/// - `format`: which of the three presentations to produce.
///
/// # Returns
/// The rendered listing as a single string.
///
/// # Errors
/// Returns an error if JSON serialization fails ([`OutputFormat::Json`]) or if
/// CSV writing / UTF-8 conversion fails ([`OutputFormat::Csv`]).
pub fn render_databases(databases: &[DatabaseInfo], format: OutputFormat) -> Result<String> {
    match format {
        OutputFormat::Table => Ok(render_database_table(databases)),
        // The JSON path builds an intermediate Vec<Value> with stable field
        // names rather than deriving Serialize on DatabaseInfo, so the on-wire
        // shape is decoupled from the internal struct layout.
        OutputFormat::Json => Ok(serde_json::to_string_pretty(
            databases_as_json(databases).as_slice(),
        )?),
        OutputFormat::Csv => render_database_csv(databases),
    }
}

/// Renders a `COUNT` aggregate result using the requested output format.
///
/// # Parameters
/// - `count`: the matched-row count to display.
/// - `format`: which of the three presentations to produce.
///
/// # Returns
/// The rendered count as a single string. JSON and CSV wrap the scalar in a
/// single-row, single-column shape (`[{"count": N}]` / a `count` header) so the
/// aggregate output is structurally consistent with the row-listing outputs.
///
/// # Errors
/// Returns an error if JSON serialization fails ([`OutputFormat::Json`]) or if
/// CSV writing / UTF-8 conversion fails ([`OutputFormat::Csv`]).
pub fn render_count(count: usize, format: OutputFormat) -> Result<String> {
    match format {
        OutputFormat::Table => Ok(render_count_table(count)),
        OutputFormat::Json => Ok(serde_json::to_string_pretty(&json!([{ "count": count }]))?),
        OutputFormat::Csv => render_count_csv(count),
    }
}

/// Prints the delete dry-run or applied summary to stdout.
///
/// Notion has no true delete; "deleted" rows are moved to the trash, hence the
/// "trashed" wording. This prints directly rather than returning a string
/// because the dry-run branch emits a multi-line preview (header plus one line
/// per affected row) that the caller never needs to capture.
///
/// # Parameters
/// - `rows`: the rows matched by the delete query.
/// - `apply`: when `true`, the deletion has already been performed and a terse
///   confirmation is printed; when `false`, a dry-run preview listing each row
///   that *would* be trashed is printed instead.
pub fn print_delete_plan(rows: &[PageRow], apply: bool) {
    if apply {
        // Matched == trashed: every matched row is trashed in one pass, so both
        // counts are intentionally the same value.
        println!("{} rows matched, {} trashed", rows.len(), rows.len());
    } else {
        println!("(dry-run) {} rows would be trashed", rows.len());
        for row in rows {
            println!("- {} ({})", row.title, row.id);
        }
    }
}

/// Renders a count aggregate as a one-cell interactive table.
///
/// # Parameters
/// - `count`: the value to place in the single `count` cell.
///
/// # Returns
/// The table text followed by the conventional `1 row matched` footer (the
/// aggregate is always a single result row).
fn render_count_table(count: usize) -> String {
    let mut table = Table::new();
    table.load_preset(UTF8_FULL);
    table.set_header(["count"]);
    table.add_row([count.to_string()]);
    format!("{table}\n1 row matched")
}

/// Renders a count aggregate as CSV (a `count` header row plus the value).
///
/// # Parameters
/// - `count`: the value to emit as the single data row.
///
/// # Returns
/// The CSV text.
///
/// # Errors
/// Returns an error if the CSV writer fails or the accumulated bytes are not
/// valid UTF-8 (they always are here, but the conversion is fallible).
fn render_count_csv(count: usize) -> Result<String> {
    let mut buffer = Vec::new();
    // The writer borrows `buffer` mutably, so it lives in an inner scope that
    // ends before we take ownership of `buffer` for the UTF-8 conversion below.
    // flush() must run while the writer is alive to push its internal buffer.
    {
        let mut writer = csv::Writer::from_writer(&mut buffer);
        writer.write_record(["count"])?;
        writer.write_record([count.to_string()])?;
        writer.flush()?;
    }

    Ok(String::from_utf8(buffer)?)
}

/// Renders visible databases as an interactive `Name`/`ID` table.
///
/// # Parameters
/// - `databases`: the databases to list as table rows.
///
/// # Returns
/// The table text followed by a `N databases matched` footer.
fn render_database_table(databases: &[DatabaseInfo]) -> String {
    let mut table = Table::new();
    table.load_preset(UTF8_FULL);
    table.set_header(["Name", "ID"]);
    for database in databases {
        table.add_row([database.name.as_str(), database.id.as_str()]);
    }
    format!("{table}\n{} databases matched", databases.len())
}

/// Renders visible databases as CSV with a `name,id` header.
///
/// # Parameters
/// - `databases`: the databases to emit, one CSV record each.
///
/// # Returns
/// The CSV text.
///
/// # Errors
/// Returns an error if the CSV writer fails or the bytes are not valid UTF-8.
fn render_database_csv(databases: &[DatabaseInfo]) -> Result<String> {
    let mut buffer = Vec::new();
    // Same scoped-writer pattern as render_count_csv: the writer must be dropped
    // before `buffer` ownership is taken for the UTF-8 conversion.
    {
        let mut writer = csv::Writer::from_writer(&mut buffer);
        writer.write_record(["name", "id"])?;
        for database in databases {
            writer.write_record([database.name.as_str(), database.id.as_str()])?;
        }
        writer.flush()?;
    }

    Ok(String::from_utf8(buffer)?)
}

/// Converts visible databases into JSON values with stable field names.
///
/// Kept separate from any `Serialize` impl on [`DatabaseInfo`] so the emitted
/// JSON shape (`{"name", "id"}`) is an explicit, stable contract independent of
/// the internal struct's fields.
///
/// # Parameters
/// - `databases`: the databases to convert.
///
/// # Returns
/// One JSON object per database, ready to be serialized as an array.
fn databases_as_json(databases: &[DatabaseInfo]) -> Vec<Value> {
    databases
        .iter()
        .map(|database| {
            json!({
                "name": database.name,
                "id": database.id
            })
        })
        .collect()
}

/// Prints the update dry-run or applied summary to stdout.
///
/// # Parameters
/// - `rows`: the rows matched by the update query.
/// - `payload`: the property payload that would be (or was) applied to each
///   matched row. Only echoed in the dry-run branch so the operator can review
///   exactly what the mutation will write before applying it.
/// - `apply`: when `true`, prints a terse confirmation; when `false`, prints a
///   preview listing each affected row followed by the pretty-printed payload.
///
/// # Returns
/// `Ok(())` once the summary is printed.
///
/// # Errors
/// Returns an error if `payload` cannot be serialized to pretty JSON.
pub fn print_update_plan(rows: &[PageRow], payload: &Value, apply: bool) -> Result<()> {
    if apply {
        // Matched == updated: the same payload is applied to every matched row.
        println!("{} rows matched, {} updated", rows.len(), rows.len());
    } else {
        println!("(dry-run) {} rows would be updated", rows.len());
        for row in rows {
            println!("- {} ({})", row.title, row.id);
        }
        println!("{}", serde_json::to_string_pretty(payload)?);
    }
    Ok(())
}

/// Prints the insert dry-run or applied summary to stdout.
///
/// Unlike update/delete there are no pre-existing rows to list, so the dry-run
/// branch echoes the full set of payloads that would be created instead.
///
/// # Parameters
/// - `payloads`: the row payloads to insert (one JSON object per new row).
/// - `apply`: when `true`, prints a terse confirmation; when `false`, prints a
///   preview with the pretty-printed payloads that would be inserted.
///
/// # Returns
/// `Ok(())` once the summary is printed.
///
/// # Errors
/// Returns an error if `payloads` cannot be serialized to pretty JSON.
pub fn print_insert_plan(payloads: &[Value], apply: bool) -> Result<()> {
    if apply {
        println!("{} rows inserted", payloads.len());
    } else {
        println!("(dry-run) {} rows would be inserted", payloads.len());
        println!("{}", serde_json::to_string_pretty(payloads)?);
    }
    Ok(())
}

/// Renders selected rows as an interactive table.
///
/// # Parameters
/// - `rows`: the rows to render as table rows.
/// - `columns`: the selected columns, defining both the header and which
///   property is extracted from each row (via [`property_string`]).
///
/// # Returns
/// The table text followed by an `N rows matched` footer.
fn render_table(rows: &[PageRow], columns: &[String]) -> String {
    let mut table = Table::new();
    table.load_preset(UTF8_FULL);
    table.set_header(columns.iter().map(Cell::new));
    for row in rows {
        table.add_row(
            columns
                .iter()
                .map(|column| Cell::new(property_string(row, column))),
        );
    }
    format!("{table}\n{} rows matched", rows.len())
}

/// Renders selected rows as JSON objects keyed by selected column name.
///
/// Each object additionally carries an `id` field (the Notion page id) so JSON
/// consumers can identify rows even when `id` was not an explicitly selected
/// column. The table and CSV renderers omit this, by design — JSON is the
/// machine-facing format where a stable identifier is most useful.
///
/// # Parameters
/// - `rows`: the rows to convert to JSON objects.
/// - `columns`: the selected columns; each becomes a string-valued field.
///
/// # Returns
/// A pretty-printed JSON array of objects.
///
/// # Errors
/// Returns an error if serialization fails.
fn render_json(rows: &[PageRow], columns: &[String]) -> Result<String> {
    let objects = rows
        .iter()
        .map(|row| {
            // serde_json::Map preserves insertion order, so `id` deliberately
            // comes first and selected columns follow in their selection order.
            let mut object = Map::new();
            object.insert("id".to_string(), Value::String(row.id.clone()));
            for column in columns {
                // Every property is emitted as a JSON string (the display form),
                // not its raw Notion JSON, so JSON/CSV/table stay consistent.
                object.insert(column.clone(), json!(property_string(row, column)));
            }
            Value::Object(object)
        })
        .collect::<Vec<_>>();
    Ok(serde_json::to_string_pretty(&objects)?)
}

/// Renders selected rows as CSV with the selected columns as the header row.
///
/// # Parameters
/// - `rows`: the rows to emit as CSV records.
/// - `columns`: the selected columns, used as both the header and the per-row
///   extraction order.
///
/// # Returns
/// The CSV text.
///
/// # Errors
/// Returns an error if the CSV writer fails or the bytes are not valid UTF-8.
fn render_csv(rows: &[PageRow], columns: &[String]) -> Result<String> {
    let mut buffer = Vec::new();
    // Same scoped-writer pattern as the other CSV renderers (see render_count_csv).
    {
        let mut writer = csv::Writer::from_writer(&mut buffer);
        writer.write_record(columns)?;
        for row in rows {
            let values = columns
                .iter()
                .map(|column| property_string(row, column))
                .collect::<Vec<_>>();
            writer.write_record(values)?;
        }
        writer.flush()?;
    }

    Ok(String::from_utf8(buffer)?)
}

/// Converts one Notion property of a row into its display string.
///
/// This is the single source of truth for how a Notion property value is shown,
/// shared across all three formats and (because it is `pub`) reused by the SQL
/// layer for filtering and ordering — so a value compares the same way it
/// displays. Each Notion property is a tagged object whose `"type"` field names
/// the active payload key (e.g. a `title` property holds its data under
/// `"title"`); this function switches on that tag and extracts accordingly.
///
/// # Parameters
/// - `row`: the page row to read the property from.
/// - `column`: the property name to look up.
///
/// # Returns
/// The display string for the property. Returns an empty string when the column
/// is absent from the row or when the typed payload is missing/null — empty is
/// treated as the universal "no value" representation across formats. Unknown or
/// untyped property kinds fall back to `value_to_display` on the whole object.
pub fn property_string(row: &PageRow, column: &str) -> String {
    // Absent column -> empty string rather than an error: a SELECT may name a
    // column some rows simply don't populate.
    let Some(property) = row.properties.get(column) else {
        return String::new();
    };

    // Dispatch on Notion's discriminant "type" tag to find the right payload key.
    match property.get("type").and_then(Value::as_str) {
        Some("title") => plain_text_array(property.get("title")),
        Some("rich_text") => plain_text_array(property.get("rich_text")),
        Some("select") => option_name(property.get("select")),
        Some("status") => option_name(property.get("status")),
        Some("multi_select") => property
            .get("multi_select")
            .and_then(Value::as_array)
            .map(|values| {
                // Multi-select values are arrays of option objects. A compact
                // comma-separated display keeps table, JSON, and CSV consistent.
                values
                    .iter()
                    .filter_map(|value| value.get("name").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default(),
        Some("number") => property
            .get("number")
            .map(value_to_display)
            .unwrap_or_default(),
        Some("checkbox") => property
            .get("checkbox")
            .and_then(Value::as_bool)
            .map(|value| value.to_string())
            .unwrap_or_default(),
        Some("date") => property
            .get("date")
            .map(date_to_display)
            .unwrap_or_default(),
        _ => value_to_display(property),
    }
}

/// Converts a Notion date object into a display string without dropping ranges or time zones.
///
/// Notion dates may be a single instant (`start` only) or a range (`start` +
/// `end`), and may carry a `time_zone`. We preserve all of it so no information
/// is silently lost in the display: a range renders as `start..end` and a time
/// zone is appended after a space.
///
/// # Parameters
/// - `value`: the Notion `date` payload object.
///
/// # Returns
/// The formatted date string, or an empty string if `start` is missing (a date
/// with no start is treated as no value).
fn date_to_display(value: &Value) -> String {
    // `start` is the only mandatory field; without it there is nothing to show.
    let Some(start) = value.get("start").and_then(Value::as_str) else {
        return String::new();
    };
    let end = value.get("end").and_then(Value::as_str);
    let time_zone = value.get("time_zone").and_then(Value::as_str);

    // Present a range with `..` only when an end is set; otherwise just the start.
    let mut display = match end {
        Some(end) => format!("{start}..{end}"),
        None => start.to_string(),
    };

    if let Some(time_zone) = time_zone {
        display.push(' ');
        display.push_str(time_zone);
    }

    display
}

/// Joins Notion rich-text fragments into a single plain-text string.
///
/// Notion stores title and rich-text properties as an array of fragments, each
/// carrying a `plain_text` field; concatenating them (with no separator) yields
/// the human-readable text. Used for both `title` and `rich_text` properties.
///
/// # Parameters
/// - `value`: the optional rich-text array (e.g. `property.get("title")`).
///
/// # Returns
/// The concatenated plain text, or an empty string if the value is absent or
/// not an array.
fn plain_text_array(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.get("plain_text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

/// Extracts the display name from a single `select`/`status` option object.
///
/// Both property kinds store their value as a single option object with a
/// `name` field (as opposed to `multi_select`, which is an array), so they share
/// this extractor.
///
/// # Parameters
/// - `value`: the optional option object (e.g. `property.get("select")`).
///
/// # Returns
/// The option's `name`, or an empty string when the option is unset or has no
/// name (e.g. a cleared select property).
fn option_name(value: Option<&Value>) -> String {
    value
        .and_then(|value| value.get("name"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Converts an arbitrary JSON value into a stable display string.
///
/// This is the fallback renderer used for `number` properties, the untyped
/// default branch of [`property_string`], and any unrecognized shape. Strings
/// are unquoted (we want the raw text, not JSON-encoded `"..."`), and `null`
/// maps to empty to match the "no value" convention used elsewhere.
///
/// # Parameters
/// - `value`: the JSON value to display.
///
/// # Returns
/// The display string for the value.
fn value_to_display(value: &Value) -> String {
    match value {
        // Null is the universal "no value" -> empty string.
        Value::Null => String::new(),
        // Return the raw text, not the JSON-quoted form produced by to_string().
        Value::String(value) => value.clone(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        // Arrays/objects have no simpler display; fall back to their JSON text.
        other => other.to_string(),
    }
}
