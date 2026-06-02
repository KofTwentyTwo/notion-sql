# Security Policy

## Supported Versions

Security fixes target the latest released version.

## Reporting A Vulnerability

Open a private security advisory on GitHub for this repository.

Do not include Notion integration tokens, page content, database IDs, or other sensitive workspace data in public issues.

## Token Handling

`notion-sql` reads the Notion integration token from `NOTION_TOKEN`.

The token must not be committed to the repository, stored in examples, or printed in logs.
