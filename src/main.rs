// SPDX-License-Identifier: MIT
// Copyright (c) 2026 James Maes
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

/// Binary entry point: drives the CLI and maps its outcome onto a process exit code.
///
/// All real work lives in [`notion_sql::cli::run`]; this function exists only to
/// translate a [`Result`] into an [`ExitCode`] and to give failures a consistent,
/// human-readable presentation on stderr.
///
/// Returning [`ExitCode`] rather than calling [`std::process::exit`] lets normal
/// stack unwinding and destructors run before the process terminates, which keeps
/// buffered output flushed and avoids the abrupt teardown `exit` would cause.
///
/// # Returns
/// [`ExitCode::SUCCESS`] when the CLI completes without error, otherwise
/// [`ExitCode::FAILURE`] after the error has been rendered to stderr.
fn main() -> ExitCode {
    match notion_sql::cli::run() {
        // Happy path: the CLI handled everything (including printing its own
        // results to stdout), so we only need to signal success to the shell.
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // Errors are surfaced on stderr (not stdout) so they stay separate
            // from query output and remain visible even when stdout is piped.
            eprintln!("{}", render_error(&error));
            ExitCode::FAILURE
        }
    }
}

/// Formats an arbitrary [`anyhow::Error`] into a single, consistently shaped
/// block of text for display on the terminal.
///
/// Two presentation strategies are used depending on the error's origin:
/// 1. If a Notion API error is found anywhere in the error chain, its own
///    purpose-built pretty renderer is preferred so API-specific guidance
///    (status codes, hints, etc.) is preserved verbatim.
/// 2. Otherwise a generic layout is produced: a header, the top-level message,
///    and — when present — an indented list of underlying causes.
///
/// # Parameters
/// - `error`: the error to format; its full chain is inspected so that nested
///   causes can be both detected (for the Notion case) and displayed (for the
///   generic case).
///
/// # Returns
/// A newline-joined [`String`] ready to print. No trailing newline is added,
/// leaving newline handling to the caller (here, `println!`/`eprintln!`).
fn render_error(error: &anyhow::Error) -> String {
    // Prefer the domain-specific renderer when the failure ultimately came from
    // the Notion API, so its richer formatting and guidance are not flattened
    // into the generic layout below.
    if let Some(notion_error) = find_notion_api_error(error) {
        return notion_error.render_pretty();
    }

    // Generic layout: a labeled header, a blank separator line, then the
    // top-level (outermost) error message. The two-space indentation and
    // aligned "Message :" label are intentional to match the "Details" block.
    let mut lines = vec![
        "notion-sql error".to_string(),
        String::new(),
        format!("  Message : {error}"),
    ];

    // `chain()` yields the error itself first; `skip(1)` drops it so we only
    // collect the *underlying* causes already summarized by the message above.
    let causes = error.chain().skip(1).collect::<Vec<_>>();
    // Only emit the "Details" section when there is at least one nested cause;
    // an empty section would be noise for simple single-layer errors.
    if !causes.is_empty() {
        lines.push(String::new());
        lines.push("  Details".to_string());
        // Render causes outermost-to-innermost (the order `chain()` provides),
        // each as a bullet so the failure's provenance is easy to scan.
        for cause in causes {
            lines.push(format!("  - {cause}"));
        }
    }

    lines.join("\n")
}
