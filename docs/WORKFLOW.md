# Development Workflow

This project uses a Git Flow-inspired workflow with `develop` as the leading edge for all development work.

## Branch Structure

- **develop**: Leading edge branch for ongoing development. All feature branches are created from this.
- **main**: Production-ready code. Only merged from develop after review and testing.
- **feature/***: Short-lived branches for specific features. Created from and merged back to develop.

## Workflow

### Starting New Work

```bash
# Ensure you're on develop and up to date
git checkout develop
git pull origin develop

# Create a feature branch from develop
git checkout -b feature/your-feature-name
```

### Completing Work

```bash
# Commit your changes
git add .
git commit -m "Describe your change"

# Push to remote (this creates the branch on origin)
git push -u origin feature/your-feature-name
```

### Creating a Pull Request

1. Push your feature branch to GitHub
2. Create a Pull Request from `feature/*` → `develop`
3. Get review and approval
4. Merge to develop

### Releasing to Production

```bash
# When ready to release, merge develop to main
git checkout main
git pull origin main
git merge develop
git push origin main

# Create a tag for the release
git tag -a v1.0.0 -m "Version 1.0.0"
git push origin v1.0.0
```

## Guidelines

- Always create feature branches from `develop`
- Never commit directly to `develop` or `main`
- Keep feature branches focused on a single purpose
- Merge feature branches to `develop` (not directly to main)
- Only merge `develop` to `main` when ready for production release
- Use meaningful branch names: `feature/`, `fix/`, `refactor/`, etc.
