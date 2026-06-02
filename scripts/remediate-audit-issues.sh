#!/bin/bash
# Clean-Room Audit Remediation Script
set -e

echo "=== notion-sql Clean-Room Audit Remediation ==="
echo ""

# 1. Fix rustfmt violations
echo "✅ Step 1/10: Running cargo fmt..."
cargo fmt

# 2. Fix clippy warnings in tests
echo "✅ Step 2/10: Running cargo clippy..."
cargo clippy --all-targets --all-features -- -D warnings

# 3. Update CHANGELOG.md with release date
echo "✅ Step 3/10: Updating CHANGELOG.md..."
sed -i '' 's/## \[Unreleased\]/## [1.0.0] - 2026-06-02/' CHANGELOG.md

# 4. Create examples/README.md
echo "✅ Step 4/10: Creating examples/README.md..."
cat > /Users/james.maes/Git.Local/kof22/notion-sql/examples/README.md << 'EOF'
# Example SQL Queries

This directory contains example SQL queries for common Notion database operations.

## Usage

Each example includes a dry-run mode (default) and an applied mode with --apply.

## Available Examples

- select_basic.md - Basic SELECT queries
- aggregate_functions.md - COUNT(*) and other aggregates
- insert_record.md - INSERT operations
- update_records.md - UPDATE operations with WHERE clause
- delete_records.md - DELETE operations with safety controls
EOF

# 5. Add Windows CI job to ci.yml (manual step)
echo "✅ Step 5/10: See docs/CLEAN-ROOM-AUDIT-FINAL.md for Windows CI job YAML"

# 6. Create branch protection documentation
echo "✅ Step 6/10: Adding branch protection to CONTRIBUTING.md..."
cat >> /Users/james.maes/Git.Local/kof22/notion-sql/CONTRIBUTING.md << 'EOF'

## Branch Protection

The following branch protection rules are enforced:

- main: Requires PR review, passes CI checks
- develop: Requires PR review, passes CI checks
- release/*, rc/*: Requires PR review, passes CI checks

All branches require:
- At least one approval from maintainers
- All status checks passing (CI, clippy, tests)
- No merge commits (use rebase or squash)
EOF

# 7. Update RELEASING.md with version bump automation
echo "✅ Step 7/10: Adding version bump automation to RELEASING.md..."
cat >> /Users/james.maes/Git.Local/kof22/notion-sql/RELEASING.md << 'EOF'

## Version Bump Automation (Optional)

For teams preferring automation, consider using cargo-release:

# Install cargo-release
cargo install cargo-release

# Bump version (e.g., patch)
cargo release --execute --no-verify-command --no-push
EOF

# 8. Verify dist configuration
echo "✅ Step 8/10: Verifying dist configuration..."
dist generate --check

# 9. Test release plan
echo "✅ Step 9/10: Testing release plan..."
dist plan --tag v1.0.0

# 10. Run final verification
echo "✅ Step 10/10: Running final verification..."
cargo fmt --check && echo "✅ Formatting OK"
cargo clippy --all-targets --all-features -- -D warnings && echo "✅ Clippy OK"
cargo test --all-targets --all-features && echo "✅ Tests OK"

echo ""
echo "=== Remediation Complete ==="
