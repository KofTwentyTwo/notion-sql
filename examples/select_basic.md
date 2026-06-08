# Basic SELECT Query

This example shows how to query a Notion database and retrieve specific columns.

```sql
SELECT Name, Status, Priority FROM Tasks WHERE Status = 'Active' ORDER BY Priority DESC;
```

This query:
- Selects three columns: Name, Status, and Priority
- Filters for records where Status equals 'Active'
- Orders results by Priority in descending order

Run with:
```bash
notion-sql query --database-id YOUR_DATABASE_ID --sql "SELECT Name, Status, Priority FROM Tasks WHERE Status = 'Active' ORDER BY Priority DESC;"
```
