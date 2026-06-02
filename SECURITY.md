# Security Policy

## Supported Versions

| Version | Supported          |
| ------- | ------------------ |
| 1.0.x   | :white_check_mark: |
| 0.1.x   | :x:                |

## Reporting A Vulnerability

**Do not open a public GitHub issue for security vulnerabilities.**

Instead, please use one of the following methods:

1. **GitHub Security Advisories**: Create a private security advisory at
   `https://github.com/KofTwentyTwo/notion-sql/security/advisories`

2. **PGP Encryption**: If you have the maintainer's PGP public key,
   encrypt your report and send it via email.

3. **Disclose to Notion**: If the vulnerability is in the Notion API itself,
   please also report it to Notion's security team at `security@notion.com`.

### What To Include

- Type of vulnerability (e.g., injection, authentication bypass)
- Steps to reproduce
- Potential impact
- Suggested fix (if any)

### Response Timeline

- Acknowledgment: Within 48 hours
- Status update: Within 7 days
- Resolution: Within 30 days (or discussion of timeline if complex)

## Security Best Practices

### Token Handling

- Never commit `NOTION_TOKEN` to version control
- Use environment variables or secure secret management
- Rotate tokens if they are exposed
- Use internal integration tokens with minimal required permissions

### Rate Limiting

The CLI implements automatic rate limiting with retries. To avoid triggering
rate limits:

- Use `--progress` to monitor long-running operations
- Consider narrowing WHERE clauses to reduce query volume
- Avoid rapid repeated queries against the same database

### API Versioning

This release targets Notion API version `2022-06-28`. Future releases may
migrate to newer API versions with appropriate breaking change handling.

## Security Features

- TLS 1.3 for all Notion API connections
- Automatic retry with exponential backoff for rate limits
- Structured error messages that never expose tokens
- Dry-run mode to preview mutations before execution

## Known Security Considerations

- The CLI caches database names in memory for the current session
- No persistent storage of Notion tokens or data
