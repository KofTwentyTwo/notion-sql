# DELETE Records Example

This example shows how to delete records from a Notion database.

```sql
DELETE FROM Tasks WHERE Status = 'Archived' AND Created < '2024-01-01';
```

This query:
- Deletes records from the Tasks database
- Only deletes archived records created before January 1, 2024

Run with:
```bash
notion-sql query --database-id YOUR_DATABASE_ID --sql "DELETE FROM Tasks WHERE Status = 'Archived' AND Created < '2024-01-01';" --execute
```

⚠️ Warning: Deletions are permanent. Always backup your data before running DELETE queries!
