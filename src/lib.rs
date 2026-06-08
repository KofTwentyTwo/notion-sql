//! Library modules for the `notion-sql` command line application.
//!
//! The crate separates SQL parsing, Notion schema and API access, value coercion,
//! filter translation, and output rendering so the binary entrypoint stays thin.

#![warn(missing_docs)]
#![warn(clippy::missing_docs_in_private_items)]

/// Command line parsing and statement execution orchestration.
pub mod cli;
/// Translation from SQL `WHERE` expressions into Notion filter JSON.
pub mod filter;
/// Blocking Notion API client and response adapters.
pub mod notion;
/// Human-readable and machine-readable output rendering.
pub mod output;
/// Notion database schema discovery and property lookup.
pub mod schema;
/// SQL parser adapter that narrows `sqlparser` output to supported statements.
pub mod sql;
/// SQL literal handling and Notion property value coercion.
pub mod value;
