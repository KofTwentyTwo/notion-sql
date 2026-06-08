// SPDX-License-Identifier: MIT
// Copyright (c) 2026 James Maes
//! Integration tests for the crate's `output` module, exercising how raw Notion
//! property values are rendered into human-readable, terminal-safe display strings.
//!
//! # What is under test
//!
//! This file drives [`notion_sql::output::property_string`], the function that
//! takes a [`PageRow`] plus a property name and produces the scalar string that
//! ends up in CLI output (tables, CSV cells, etc.). Notion's API returns property
//! values as loosely-typed JSON discriminated by a `"type"` tag, so the rendering
//! logic must branch on that tag and flatten nested structures into a single line.
//!
//! # Testing approach
//!
//! Each test constructs a [`PageRow`] by hand with a deliberately crafted
//! `properties` map (rather than hitting the live Notion API), then asserts on the
//! exact rendered string. Building the JSON inline keeps the test hermetic and lets
//! us pin down the precise output format — including separators and ordering — that
//! downstream tooling and users depend on.
//!
//! As an integration test (in `tests/`) it can only reach the crate's public API,
//! so it documents the contract `output` exposes to the rest of the binary.

use notion_sql::notion::PageRow;
use serde_json::{json, Map};

/// Verifies that a Notion `date` property carrying a start/end range plus an
/// explicit time zone is flattened into the canonical single-line form
/// `"<start>..<end> <time_zone>"`.
///
/// This guards the most information-dense date shape Notion can return: a closed
/// interval (`start` and `end` both present) annotated with an IANA `time_zone`.
/// The expected output pins down two format decisions that callers rely on — the
/// `..` separator between the two endpoints and the single space before the time
/// zone — so a regression in either would fail here rather than silently corrupt
/// rendered tables.
///
/// # Panics
///
/// Panics (failing the test) if [`notion_sql::output::property_string`] does not
/// return the exact expected range-with-time-zone string.
#[test]
fn renders_date_ranges_and_time_zones() {
    // Construct a minimal page by hand: id/title are required by the struct but
    // irrelevant to date rendering, so they hold placeholder values. Only the
    // "Due" property carries the payload exercised by this test.
    let row = PageRow {
        id: "page-id".to_string(),
        title: "Task".to_string(),
        // `properties` mirrors Notion's API shape: a map from property name to a
        // type-tagged JSON object. Here the single entry is the date range under test.
        properties: Map::from_iter([(
            "Due".to_string(),
            // The `"type": "date"` tag is what `property_string` dispatches on; the
            // nested `date` object supplies start, end, and time_zone in one value.
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
        notion_sql::output::property_string(&row, "Due"),
        "2026-06-01T09:00:00.000-05:00..2026-06-01T10:00:00.000-05:00 America/Chicago"
    );
}
