//! Tests for parsing supported SQL into the local statement model.

use notion_sql::sql::{parse_statement, SelectColumns, SqlStatement};

#[test]
fn parses_select_statement() {
    let parsed = parse_statement(
        "SELECT Name, Status FROM Tasks WHERE Status = 'Done' ORDER BY Name DESC LIMIT 5",
    )
    .unwrap();

    match parsed {
        SqlStatement::Select {
            database,
            columns,
            sorts,
            limit,
            ..
        } => {
            assert_eq!(database, "Tasks");
            assert_eq!(
                columns,
                SelectColumns::Columns(vec!["Name".to_string(), "Status".to_string()])
            );
            assert_eq!(
                sorts,
                vec![notion_sql::sql::SortSpec {
                    column: "Name".to_string(),
                    ascending: false
                }]
            );
            assert_eq!(limit, Some(5));
        }
        other => panic!("unexpected statement: {other:?}"),
    }
}

#[test]
fn parses_insert_values() {
    let parsed =
        parse_statement("INSERT INTO Tasks (Name, Status) VALUES ('New task', 'To Do')").unwrap();

    match parsed {
        SqlStatement::Insert {
            database,
            columns,
            rows,
        } => {
            assert_eq!(database, "Tasks");
            assert_eq!(columns, vec!["Name".to_string(), "Status".to_string()]);
            assert_eq!(rows.len(), 1);
        }
        other => panic!("unexpected statement: {other:?}"),
    }
}

#[test]
fn parses_count_projection() {
    let parsed = parse_statement("SELECT COUNT(*) FROM Tasks WHERE Status = 'Done'").unwrap();

    match parsed {
        SqlStatement::Select {
            database, columns, ..
        } => {
            assert_eq!(database, "Tasks");
            assert_eq!(columns, SelectColumns::Count);
        }
        other => panic!("unexpected statement: {other:?}"),
    }
}

#[test]
fn rejects_unsupported_count_projection() {
    let error = parse_statement("SELECT COUNT(Name) FROM Tasks").unwrap_err();

    assert!(error.to_string().contains("Only COUNT(*) and COUNT(1)"));
}
