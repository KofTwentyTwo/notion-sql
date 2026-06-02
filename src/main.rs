//! `notion-sql` runs SQL-style CRUD statements against Notion databases.
//!
//! Examples:
//! - `notion-sql "SELECT Name, Status FROM Tasks WHERE Status='Done'"`
//! - `notion-sql "UPDATE Tasks SET Status='Archived' WHERE Priority='Low'" --apply`
//!
//! Set `NOTION_TOKEN` to an internal integration token and share target
//! databases with that integration before running queries.

#![warn(missing_docs)]
#![warn(clippy::missing_docs_in_private_items)]

use std::process::ExitCode;

use notion_sql::notion::find_notion_api_error;

/// Starts the CLI, renders polished errors, and returns the process exit code.
fn main() -> ExitCode {
    match notion_sql::cli::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{}", render_error(&error));
            ExitCode::FAILURE
        }
    }
}

/// Formats all user-facing errors with a consistent terminal shape.
fn render_error(error: &anyhow::Error) -> String {
    if let Some(notion_error) = find_notion_api_error(error) {
        return notion_error.render_pretty();
    }

    let mut lines = vec![
        "notion-sql error".to_string(),
        String::new(),
        format!("  Message : {error}"),
    ];

    let causes = error.chain().skip(1).collect::<Vec<_>>();
    if !causes.is_empty() {
        lines.push(String::new());
        lines.push("  Details".to_string());
        for cause in causes {
            lines.push(format!("  - {cause}"));
        }
    }

    lines.join("\n")
}
