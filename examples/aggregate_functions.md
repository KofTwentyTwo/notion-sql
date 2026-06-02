# Aggregate Functions Example

This example shows how to use aggregate functions in your queries.

```sql
SELECT 
    Status,
    COUNT(*) as count,
    AVG(Rating) as average_rating
FROM Tasks 
GROUP BY Status;
```

This query:
- Groups records by Status column
- Counts the number of records in each status group
- Calculates the average Rating for each status

Run with:
```bash
notion-sql query --database-id YOUR_DATABASE_ID --sql "SELECT Status, COUNT(*) as count, AVG(Rating) as average_rating FROM Tasks GROUP BY Status;"
```

Supported aggregate functions:
- COUNT(*) - Count the number of records
- SUM(column) - Sum values in a numeric column
- AVG(column) - Calculate the average of a numeric column
- MIN(column) - Find the minimum value
- MAX(column) - Find the maximum value
