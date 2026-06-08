// SPDX-License-Identifier: MIT
// Copyright (c) 2026 James Maes
//! Library modules for the `notion-sql` command line application.
//!
//! The crate separates SQL parsing, Notion schema and API access, value coercion,
//! filter translation, and output rendering so the binary entrypoint stays thin.

#![warn(missing_docs)]
#![warn(clippy::missing_docs_in_private_items)]

pub mod cli;
pub mod filter;
pub mod notion;
pub mod output;
pub mod schema;
pub mod sql;
pub mod value;
