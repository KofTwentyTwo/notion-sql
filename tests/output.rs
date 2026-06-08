//! Tests for rendering Notion values into terminal-safe display strings.

use notion_sql::notion::PageRow;
use serde_json::{json, Map};

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
        notion_sql::output::property_string(&row, "Due"),
        "2026-06-01T09:00:00.000-05:00..2026-06-01T10:00:00.000-05:00 America/Chicago"
    );
}
