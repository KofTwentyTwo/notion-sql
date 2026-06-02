# Release Procedure for notion-sql

## Prerequisites

Before releasing, ensure:
1. All tests pass: `cargo test --all-targets`
2. Code is clean: `cargo clippy --all-targets --all-features -- -D warnings`
3. Formatting is correct: `cargo fmt --check`

## Release Steps

### 1. Update Version
- Bump version in `Cargo.toml` (e.g., `version = "1.0.0"`)
- Update `CHANGELOG.md` with release notes following Keep a Changelog format

### 2. Commit Changes
```bash
git add -A
git commit -m "chore: Update to v$VERSION"
```

### 3. Create Release Tag
```bash
git tag -a v$VERSION -m "notion-sql v$VERSION - [release notes]"
git push origin v$VERSION
```

### 4. Verify Release Workflow
- Check GitHub Actions for the Release workflow run
- Wait for all 5 platform binaries to build:
  - macOS ARM64 (aarch64-apple-darwin)
  - macOS x86_64 (x86_64-apple-darwin)
  - Linux ARM64 (aarch64-unknown-linux-gnu)
  - Linux x86_64 (x86_64-unknown-linux-gnu)
  - Windows x86_64 (x86_64-pc-windows-msvc)

### 5. Publish Release
Once the workflow completes successfully:
```bash
gh release edit v$VERSION --draft=false
```

## Platform Targets Configuration

The `Cargo.toml` `[workspace.metadata.dist]` section configures all targets:

```toml
targets = [
  "aarch64-apple-darwin",
  "aarch64-unknown-linux-gnu", 
  "x86_64-apple-darwin",
  "x86_64-unknown-linux-gnu",
  "x86_64-pc-windows-msvc"
]
```

## Homebrew Integration

The release workflow automatically:
1. Builds Homebrew formula (`notion-sql.rb`)
2. Pushes to `KofTwentyTwo/homebrew-tap` repository
3. Users install via: `brew install KofTwentyTwo/tap/notion-sql`

## Security

- Release tags are protected via repository ruleset
- Branch protection on `main` prevents force pushes and deletions
- HOMEBREW_TAP_TOKEN secret is required for Homebrew formula publishing

## Troubleshooting

### Release Workflow Fails
- Check GitHub Actions logs for the Release workflow
- Common issues:
  - Tag already exists (delete and recreate)
  - Missing HOMEBREW_TAP_TOKEN secret
  - Homebrew tap repository not accessible

### Binaries Not Built
- Verify `targets` in Cargo.toml includes all desired platforms
- Check CI/CD workflow has permission to build all targets
