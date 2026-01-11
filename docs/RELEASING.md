# Releasing Temps

This document describes the release process for Temps.

## Automated Release Process

Releases are automated via GitHub Actions. When you push a version tag, it will:

1. Create a GitHub release
2. Build the `temps` binary for Linux AMD64
3. Include web UI in the binary (built automatically)
4. Attach the binary to the release
5. Generate and attach SHA256 checksums

## Creating a Release

### 1. Update Version

Update version in relevant files:
- `Cargo.toml` (workspace version)
- `crates/temps-cli/Cargo.toml`

### 2. Update CHANGELOG.md

Create or update `CHANGELOG.md` with changes for this release:

```markdown
## [1.0.0] - 2025-10-22

### Added
- Feature 1
- Feature 2

### Changed
- Change 1

### Fixed
- Bug fix 1
```

### 3. Commit Changes

```bash
git add .
git commit -m "chore: bump version to v1.0.0"
git push origin main
```

### 4. Create and Push Tag

```bash
# Create tag (replace with your version)
git tag v1.0.0

# Push tag to trigger release
git push origin v1.0.0
```

**Tag Format:**
- `v1.0.0` - Stable release
- `v1.0.0-beta.1` - Pre-release (marked as prerelease on GitHub)
- `v1.0.0-rc.1` - Release candidate (marked as prerelease)

### 5. Monitor Release

1. Go to the **Actions** tab in GitHub
2. Watch the **Release** workflow
3. When complete, check the **Releases** page

## Release Artifacts

Each release includes:

- `temps-linux-amd64` - Linux AMD64 binary
- `temps-linux-amd64.sha256` - SHA256 checksum

### Verifying Release

After release, verify the binary works:

```bash
# Download
curl -LO https://github.com/YOUR_ORG/temps/releases/download/v1.0.0/temps-linux-amd64

# Verify checksum
curl -LO https://github.com/YOUR_ORG/temps/releases/download/v1.0.0/temps-linux-amd64.sha256
sha256sum -c temps-linux-amd64.sha256

# Make executable
chmod +x temps-linux-amd64

# Test
./temps-linux-amd64 --version
```

## Multi-Platform Releases (Optional)

To build for multiple platforms (Linux AMD64/ARM64, macOS Intel/Apple Silicon):

1. Rename `.github/workflows/release-multi-platform.yml.disabled` to `.github/workflows/release-multi-platform.yml`
2. Delete or disable `.github/workflows/release.yml`
3. Push a new tag

This will build for:
- Linux AMD64 (`x86_64-unknown-linux-gnu`)
- Linux ARM64 (`aarch64-unknown-linux-gnu`)
- macOS Intel (`x86_64-apple-darwin`)
- macOS Apple Silicon (`aarch64-apple-darwin`)

## Troubleshooting

### Release Failed

Check the GitHub Actions logs:
1. Go to **Actions** tab
2. Click the failed workflow run
3. Expand the failed job to see logs

Common issues:
- **Bun not found**: Ensure bun is installed in the workflow
- **Web build failed**: Check web dependencies and build script
- **Rust compilation error**: Fix the code and push a new tag

### Delete Failed Release

```bash
# Delete tag locally
git tag -d v1.0.0

# Delete tag remotely
git push origin :refs/tags/v1.0.0

# Delete release on GitHub (via web UI or gh cli)
gh release delete v1.0.0
```

### Create Hotfix Release

For urgent fixes:

```bash
# Create hotfix from tag
git checkout v1.0.0
git checkout -b hotfix-1.0.1

# Make fixes
git commit -m "fix: critical bug"

# Tag and push
git tag v1.0.1
git push origin v1.0.1

# Merge back to main
git checkout main
git merge hotfix-1.0.1
git push origin main
```

## Release Checklist

Before creating a release:

- [ ] All tests pass (`cargo test --workspace`)
- [ ] Clippy passes (`cargo clippy --workspace -- -D warnings`)
- [ ] Version updated in Cargo.toml files
- [ ] CHANGELOG.md updated
- [ ] Web UI builds successfully (`cd web && bun run build`)
- [ ] Documentation updated if needed
- [ ] Breaking changes documented (if any)

## Manual Release (Emergency)

If automation fails, you can create a manual release:

```bash
# Build binary
FORCE_WEB_BUILD=1 cargo build --release --bin temps

# Strip binary
strip target/release/temps

# Create release on GitHub
gh release create v1.0.0 \
  --title "Release v1.0.0" \
  --notes "See CHANGELOG.md" \
  target/release/temps#temps-linux-amd64
```

## Versioning

Temps follows [Semantic Versioning](https://semver.org/):

- **MAJOR** version: Incompatible API changes
- **MINOR** version: New functionality (backwards compatible)
- **PATCH** version: Bug fixes (backwards compatible)

Examples:
- `v1.0.0` → `v1.0.1` - Bug fix
- `v1.0.0` → `v1.1.0` - New feature
- `v1.0.0` → `v2.0.0` - Breaking change
