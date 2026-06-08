# Developer convenience wrapper. Every recipe maps to a raw cargo command.
set shell := ["bash", "-cu"]

# Fast local gate — mirrors .githooks/pre-commit.
check: fmt-check lint headers

# Formatting (check only / apply).
fmt-check:
    cargo fmt --check
fmt:
    cargo fmt

# Clippy with warnings denied across all targets.
lint:
    cargo clippy --all-targets --all-features -- -D warnings

# Full test suite.
test:
    cargo test --all-targets --all-features

# SPDX license-header check.
headers:
    ./scripts/check-headers.sh

# Supply-chain / license gate.
deny:
    cargo deny check

# Docs with broken-link detection.
doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --no-deps

# One-time per clone: point git at the committed hooks.
setup:
    git config core.hooksPath .githooks
    @echo "Hooks installed (core.hooksPath = .githooks)."
