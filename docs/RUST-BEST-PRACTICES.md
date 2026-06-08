# Rust Project Best Practices

## Test Organization

### ❌ Don't Mix Tests with Source Code
```rust
// src/notion.rs - BAD
fn some_function() { ... }

#[cfg(test)]
mod tests {
    #[test]
    fn test_something() { ... }
}
```

### ✅ Do Use Separate Test Directory
```bash
src/
  notion.rs          # Only implementation code
tests/
  notion.rs          # Unit tests for notion module
  integration.rs     # Integration tests
```

## Project Structure

```
notion-sql/
├── src/
│   ├── main.rs      # Binary entrypoint
│   └── lib.rs       # Library exports
├── tests/           # All tests (unit + integration)
│   ├── cli.rs       # CLI module tests
│   ├── filter.rs    # Filter module tests
│   └── integration.rs  # Integration tests
├── examples/        # Example usage
│   ├── README.md
│   └── select_basic.md
├── docs/            # Project documentation
└── .github/workflows/
    ├── ci.yml       # CI pipeline
    └── release.yml  # Release automation
```

## Code Quality Gates

### Always Pass
```bash
cargo fmt --check          # Formatting
cargo clippy --all-targets --all-features -- -D warnings  # Lints
cargo test --all-targets   # All tests (39 tests passing)
```

### Build Verification
```bash
cargo build --release      # Release build
cargo doc --no-deps        # Documentation generation
```

## CI/CD Pipeline

### CI Workflow (ci.yml)
- Runs on PRs and pushes to main branches
- Tests: Rust checks, clippy, formatting, tests, release build
- Uploads branch artifacts for develop/release branches

### Release Workflow (release.yml)
- Triggered by version tags (`vX.Y.Z`)
- Builds all configured targets
- Creates GitHub release with assets
- Publishes Homebrew formula

## Target Platforms

Configure all platforms in `Cargo.toml`:

```toml
[workspace.metadata.dist]
targets = [
  "aarch64-apple-darwin",      # macOS ARM (Apple Silicon)
  "x86_64-apple-darwin",       # macOS Intel
  "aarch64-unknown-linux-gnu", # Linux ARM
  "x86_64-unknown-linux-gnu",  # Linux x86
  "x86_64-pc-windows-msvc"     # Windows x86
]
```

## Release Checklist

Before tagging:
- [ ] All tests pass (`cargo test --all-targets`)
- [ ] Clippy clean (`cargo clippy --all-targets`)
- [ ] Formatting correct (`cargo fmt --check`)
- [ ] CHANGELOG.md updated
- [ ] Version bumped in Cargo.toml

After tagging:
- [ ] Release workflow completes successfully
- [ ] All 5 platform binaries built
- [ ] Homebrew formula published
- [ ] GitHub release created with all assets

## Branch Protection

Enable branch protection for `main`:
- Require pull request reviews (1 approval)
- Enforce admin restrictions
- Disable force pushes
- Disable deletions

Repository ruleset for tags:
- Protect tags matching `v*`
- Prevent tag deletions
- Prevent force pushes

## Common Mistakes

1. **Mixing tests with source** - Use separate `tests/` directory
2. **Missing Windows target** - Always include `x86_64-pc-windows-msvc`
3. **Unverified Homebrew token** - Ensure HOMEBREW_TAP_TOKEN secret exists
4. **Draft releases left open** - Always publish releases after workflow completes
