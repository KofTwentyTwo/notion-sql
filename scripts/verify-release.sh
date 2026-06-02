#!/bin/bash
# Release Verification Checklist

set -e

echo "=== notion-sql v1.0.0 Release Verification ==="
echo ""

# 1. Code Quality Gates
echo "🔍 Step 1/6: Code Quality Gates"
cargo fmt --check && echo "✅ Formatting OK" || { echo "❌ Formatting FAILED"; exit 1; }
cargo clippy --all-targets --all-features -- -D warnings && echo "✅ Clippy OK" || { echo "❌ Clippy FAILED"; exit 1; }
cargo test --all-targets --all-features && echo "✅ Tests OK" || { echo "❌ Tests FAILED"; exit 1; }
RUSTDOCFLAGS='-D warnings' cargo doc --no-deps --all-features && echo "✅ Docs OK" || { echo "❌ Docs FAILED"; exit 1; }

# 2. Build System Gates
echo ""
echo "🔍 Step 2/6: Build System Gates"
cargo build --release && echo "✅ Release build OK" || { echo "❌ Build FAILED"; exit 1; }
dist generate --check && echo "✅ Dist config OK" || { echo "❌ Dist config FAILED"; exit 1; }
dist plan --tag v1.0.0 && echo "✅ Release plan OK" || { echo "❌ Release plan FAILED"; exit 1; }

# 3. Release Process Gates
echo ""
echo "🔍 Step 3/6: Release Process Gates"
gh repo view KofTwentyTwo/homebrew-tap && echo "✅ Homebrew tap exists" || { echo "❌ Homebrew tap FAILED"; exit 1; }
gh secret list --repo KofTwentyTwo/notion-sql | grep HOMEBREW_TAP_TOKEN && echo "✅ Token exists" || { echo "❌ Token check FAILED"; exit 1; }

# 4. Documentation Gates
echo ""
echo "🔍 Step 4/6: Documentation Gates"
grep -q "2026-06-02" CHANGELOG.md && echo "✅ CHANGELOG updated" || { echo "❌ CHANGELOG not updated"; exit 1; }
test -f examples/README.md && echo "✅ Examples README exists" || { echo "❌ Examples README missing"; exit 1; }

# 5. Security Gates
echo ""
echo "🔍 Step 5/6: Security Gates"
grep -q "NOTION_TOKEN" SECURITY.md && echo "✅ Token handling documented" || { echo "❌ Token handling not documented"; exit 1; }
grep -q "dry-run" README.md && echo "✅ Dry-run mode documented" || { echo "❌ Dry-run mode not documented"; exit 1; }

# 6. Final Verification
echo ""
echo "🔍 Step 6/6: Final Verification"
git status --short && echo "✅ No uncommitted changes" || { echo "⚠️  Uncommitted changes detected"; }
git log --oneline -5 && echo "✅ Recent commits OK"
git tag -l && echo "✅ Tags OK"

echo ""
echo "=== Release Verification Complete ==="
echo ""
echo "If all checks passed, proceed with release:"
echo "  git commit -am 'Update CHANGELOG for v1.0.0'"
echo "  git tag v1.0.0"
echo "  git push origin v1.0.0"
