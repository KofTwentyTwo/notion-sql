Going with pre-built binaries — that's cargo-dist's sweet spot, and it auto-generates the Homebrew formula and pushes it to your tap on every tag, so there's no formula to hand-maintain. Append this section to the prompt:

## Add to the agent prompt — release & Homebrew automation

---

**Set up automated multi-platform releases and Homebrew publishing using `cargo-dist` (the `dist` tool).**

- Run `cargo dist init` and configure it in the project so that tagging a release builds pre-built binaries and publishes them.
- Build targets: `aarch64-apple-darwin` and `x86_64-apple-darwin` (Apple Silicon + Intel Mac), plus `x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu` for Linux users.
- Enable the **Homebrew installer**: in the dist config (`[workspace.metadata.dist]` or `dist-workspace.toml`), set `installers = ["shell", "homebrew"]` and point `tap = "<GITHUB_USER>/homebrew-tap"` so dist generates a `notion-sql.rb` formula and pushes it to that tap repo on each release. Use `<GITHUB_USER>` as a placeholder I'll replace.
- Generate the GitHub Actions release workflow (`.github/workflows/release.yml`) that triggers on `v*` tags: it should build all target binaries, create a GitHub Release with the tarballs + checksums, and publish the Homebrew formula to the tap.
- The Homebrew publish step needs a token with push access to the tap repo — reference it as a repo secret named `HOMEBREW_TAP_TOKEN` in the workflow (do not hardcode anything). Document this in the README.
- Add a `RELEASING.md` documenting the release flow: bump the version in `Cargo.toml`, commit, `git tag vX.Y.Z`, `git push --tags` → CI does the rest. Include the exact end-user install command: `brew install <GITHUB_USER>/tap/notion-sql`.
- Ensure `Cargo.toml` has all metadata dist needs (version, description, license, repository).

---

The two manual steps you'll own (the agent can't do these, they need your GitHub account):

1. **Create the tap repo** — a public repo named exactly `homebrew-tap` under your GitHub account. dist pushes the formula there; the `homebrew-` prefix is what makes `brew install you/tap/notion-sql` resolve.
2. **Add the `HOMEBREW_TAP_TOKEN` secret** — a fine-grained PAT (or classic token) with contents write access to that tap repo, saved as an Actions secret on the `notion-sql` repo. Needed only because the release workflow pushes to a *different* repo than the one it runs in.

After that, your whole release loop is: bump version → tag → push. CI builds every binary, cuts the GitHub release, and updates the formula automatically.

That's the full prompt assembled across all four pieces (core tool, SQL layer, Rust, release/brew). Want me to drop the entire thing into a single clean `PROMPT.md` file you can hand to the agent?
