# INSERT Record Example

This example shows how to insert a new record into a Notion database.

```sql
INSERT INTO Tasks (Name, Status, Priority) VALUES ('Review PR', 'Active', 'High');
```

This query:
- Inserts a new record into the Tasks database
- Sets Name to 'Review PR', Status to 'Active', and Priority to 'High'

Run with:
```bash
notion-sql query --database-id YOUR_DATABASE_ID --sql "INSERT INTO Tasks (Name, Status, Priority) VALUES ('Review PR', 'Active', 'High');"
```

Note: By default, notion-sql runs in dry-run mode. Add `--execute` to actually create the record:
```bash
notion-sql query --database-id YOUR_DATABASE_ID --sql "INSERT INTO Tasks (Name, Status, Priority) VALUES ('Review PR', 'Active', 'High');" --execute
```
