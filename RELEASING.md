# Releasing

Releases are built with `dist` and published by GitHub Actions.

## Branch Flow

Use the repository's gitflow-style branches:

- `feature/*` branches hold active work and open PRs into `develop`.
- `develop` receives merged feature work and runs build/test CI plus branch artifact publishing.
- `release/*` or `rc/*` branches stabilize release candidates and run the same CI plus branch artifact publishing.
- `main` is production and should only receive release-ready changes.

Do not push release tags from a workstation until GitHub authentication has been verified. This checkout currently uses the SSH remote `git@github.com:KofTwentyTwo/notion-sql.git`, and the last local verification failed because the SSH agent could not sign the configured GitHub key even though `gh auth status` was logged in as `KofTwentyTwo`. Fix SSH agent access or intentionally switch the GitHub CLI git protocol to HTTPS, then verify with:

```bash
git ls-remote --heads origin
```

## Local Release Prep

1. Update `version` in `Cargo.toml`.
2. Run:

   ```bash
   cargo fmt
   cargo clippy --all-targets --all-features -- -D warnings
   cargo test
   RUSTDOCFLAGS='-D warnings' cargo doc --no-deps --all-features
   cargo build --release
   cargo publish --dry-run --allow-dirty
   dist generate --check
   dist plan --tag vX.Y.Z
   ```

3. Commit the version bump.

## Release Candidate

From a release candidate branch:

```bash
git tag vX.Y.Z-rc.N
git push origin vX.Y.Z-rc.N
```

The `Release` workflow builds all configured target binaries and creates a GitHub prerelease.

## Production Release

From `main`:

```bash
git tag vX.Y.Z
git push origin vX.Y.Z
```

The `Release` workflow builds all configured target binaries, creates a GitHub Release, and publishes the Homebrew formula to:

```text
KofTwentyTwo/homebrew-tap
```

End users install with:

```bash
brew install KofTwentyTwo/tap/notion-sql
```

## Required GitHub Setup

Create or verify a public Homebrew tap repository:

```text
KofTwentyTwo/homebrew-tap
```

Add a repository Actions secret on `KofTwentyTwo/notion-sql` named:

```text
HOMEBREW_TAP_TOKEN
```

The token needs contents write access to `KofTwentyTwo/homebrew-tap`, because the release workflow checks out that repository and commits formula updates. Do not hardcode this token in workflow files.

Before the first production release:

- Protect or manage the `main` and `develop` branches according to the branch flow above.
- Confirm `HOMEBREW_TAP_TOKEN` can write to `KofTwentyTwo/homebrew-tap`.
- Confirm the local push path works with `git ls-remote --heads origin`.
- Re-run `actionlint`, `dist generate --check`, and `dist plan --tag vX.Y.Z`.

## Notion API Compatibility

`v0.1.0` intentionally ships against `Notion-Version: 2022-06-28` and the legacy database endpoints. This version does not use the `2025-09-03` data source APIs.

The known limitation is that Notion workspaces using the newer multi-source database model can hit API validation or capability limits in `v0.1.0`. A future release should migrate database discovery and querying to data sources before changing the pinned Notion API version.
