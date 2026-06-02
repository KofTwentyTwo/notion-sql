# Example SQL Queries

This directory contains example SQL queries for common Notion database operations.

## Available Examples

- **select_basic.md** - Basic SELECT queries with filtering and ordering
- **insert_record.md** - INSERT records into a database
- **update_records.md** - UPDATE multiple records with WHERE clauses
- **delete_records.md** - DELETE records safely
- **aggregate_functions.md** - COUNT(*), AVG, SUM, MIN, MAX with GROUP BY

## Running Examples

Each example file contains a runnable command. For example:

```bash
notion-sql query --database-id YOUR_DATABASE_ID --sql "SELECT Name, Status FROM Tasks WHERE Status = 'Active';"
```

## Prerequisites

1. Set your NOTION_TOKEN environment variable
2. Get your database ID from Notion URL (after `/v1/`)
3. Run with `--execute` flag to actually modify data (default is dry-run mode)

## Help

```bash
notion-sql --help
notion-sql query --help
```
