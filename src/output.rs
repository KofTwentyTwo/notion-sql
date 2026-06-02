//! Output rendering for query results, database listings, and mutation plans.
//!
//! Renderers return strings where possible so tests and callers can inspect the
//! final output before it is printed.

use anyhow::Result;
use comfy_table::{presets::UTF8_FULL, Cell, Table};
use serde_json::{json, Map, Value};

use crate::notion::{DatabaseInfo, PageRow};

/// Supported output formats for list and select operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    /// Render a UTF-8 table for interactive terminal use.
    Table,
    /// Render pretty JSON for scripting and inspection.
    Json,
    /// Render CSV for spreadsheet and shell pipelines.
    Csv,
}

/// Renders selected page rows using the requested output format.
pub fn render_select(rows: &[PageRow], columns: &[String], format: OutputFormat) -> Result<String> {
    match format {
        OutputFormat::Table => Ok(render_table(rows, columns)),
        OutputFormat::Json => render_json(rows, columns),
        OutputFormat::Csv => render_csv(rows, columns),
    }
}

/// Renders database metadata using the requested output format.
pub fn render_databases(databases: &[DatabaseInfo], format: OutputFormat) -> Result<String> {
    match format {
        OutputFormat::Table => Ok(render_database_table(databases)),
        OutputFormat::Json => Ok(serde_json::to_string_pretty(
            databases_as_json(databases).as_slice(),
        )?),
        OutputFormat::Csv => render_database_csv(databases),
    }
}

/// Renders a `COUNT` aggregate result using the requested output format.
pub fn render_count(count: usize, format: OutputFormat) -> Result<String> {
    match format {
        OutputFormat::Table => Ok(render_count_table(count)),
        OutputFormat::Json => Ok(serde_json::to_string_pretty(&json!([{ "count": count }]))?),
        OutputFormat::Csv => render_count_csv(count),
    }
}

/// Prints the delete dry-run or applied summary.
pub fn print_delete_plan(rows: &[PageRow], apply: bool) {
    if apply {
        println!("{} rows matched, {} trashed", rows.len(), rows.len());
    } else {
        println!("(dry-run) {} rows would be trashed", rows.len());
        for row in rows {
            println!("- {} ({})", row.title, row.id);
        }
    }
}

/// Renders a count aggregate as a one-cell interactive table.
fn render_count_table(count: usize) -> String {
    let mut table = Table::new();
    table.load_preset(UTF8_FULL);
    table.set_header(["count"]);
    table.add_row([count.to_string()]);
    format!("{table}\n1 row matched")
}

/// Renders a count aggregate as CSV.
fn render_count_csv(count: usize) -> Result<String> {
    let mut buffer = Vec::new();
    {
        let mut writer = csv::Writer::from_writer(&mut buffer);
        writer.write_record(["count"])?;
        writer.write_record([count.to_string()])?;
        writer.flush()?;
    }

    Ok(String::from_utf8(buffer)?)
}

/// Renders visible databases as an interactive table.
fn render_database_table(databases: &[DatabaseInfo]) -> String {
    let mut table = Table::new();
    table.load_preset(UTF8_FULL);
    table.set_header(["Name", "ID"]);
    for database in databases {
        table.add_row([database.name.as_str(), database.id.as_str()]);
    }
    format!("{table}\n{} databases matched", databases.len())
}

/// Renders visible databases as CSV.
fn render_database_csv(databases: &[DatabaseInfo]) -> Result<String> {
    let mut buffer = Vec::new();
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

/// Prints the update dry-run or applied summary.
pub fn print_update_plan(rows: &[PageRow], payload: &Value, apply: bool) -> Result<()> {
    if apply {
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

/// Prints the insert dry-run or applied summary.
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
fn render_json(rows: &[PageRow], columns: &[String]) -> Result<String> {
    let objects = rows
        .iter()
        .map(|row| {
            let mut object = Map::new();
            object.insert("id".to_string(), Value::String(row.id.clone()));
            for column in columns {
                object.insert(column.clone(), json!(property_string(row, column)));
            }
            Value::Object(object)
        })
        .collect::<Vec<_>>();
    Ok(serde_json::to_string_pretty(&objects)?)
}

/// Renders selected rows as CSV with the selected columns as the header row.
fn render_csv(rows: &[PageRow], columns: &[String]) -> Result<String> {
    let mut buffer = Vec::new();
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

/// Converts one Notion property value into its display string.
fn property_string(row: &PageRow, column: &str) -> String {
    let Some(property) = row.properties.get(column) else {
        return String::new();
    };

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
fn date_to_display(value: &Value) -> String {
    let Some(start) = value.get("start").and_then(Value::as_str) else {
        return String::new();
    };
    let end = value.get("end").and_then(Value::as_str);
    let time_zone = value.get("time_zone").and_then(Value::as_str);

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

/// Joins Notion rich text fragments into plain text for terminal output.
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

/// Extracts the display name from a select or status option object.
fn option_name(value: Option<&Value>) -> String {
    value
        .and_then(|value| value.get("name"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Converts a JSON scalar or object into a stable display string.
fn value_to_display(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(value) => value.clone(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    //! Tests for rendering Notion values into terminal-safe display strings.

    use serde_json::json;

    use super::*;

    /// Verifies date ranges and time zones are retained in display output.
    #[test]
    fn renders_date_ranges_and_time_zones() {
        let row = PageRow {
            id: "page-id".to_string(),
            title: "Task".to_string(),
            properties: Map::from_iter([(
                "Due".to_string(),
                json!({
                    "type": "date",
                    "date": {
                        "start": "2026-06-01T09:00:00.000-05:00",
                        "end": "2026-06-01T10:00:00.000-05:00",
                        "time_zone": "America/Chicago"
                    }
                }),
            )]),
        };

        assert_eq!(
            property_string(&row, "Due"),
            "2026-06-01T09:00:00.000-05:00..2026-06-01T10:00:00.000-05:00 America/Chicago"
        );
    }
}
