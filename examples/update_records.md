# UPDATE Records Example

This example shows how to update multiple records in a Notion database.

```sql
UPDATE Tasks SET Status = 'Done' WHERE Priority = 'Low';
```

This query:
- Updates all records in the Tasks database where Priority is 'Low'
- Sets their Status to 'Done'

Run with:
```bash
notion-sql query --database-id YOUR_DATABASE_ID --sql "UPDATE Tasks SET Status = 'Done' WHERE Priority = 'Low';" --execute
```

⚠️ Warning: Always test your WHERE clause first with a SELECT query to ensure you're updating the correct records!
