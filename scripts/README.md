# Scripts

Helper scripts for Temps development and operations.

## Release Script

`release.sh` - Automates the release process.

### Usage

```bash
# Interactive mode (prompts for version)
./scripts/release.sh

# With version argument
./scripts/release.sh 1.0.0
```

### What it does

1. ✅ Checks you're on the main branch
2. ✅ Verifies no uncommitted changes
3. ✅ Runs tests (`cargo test --workspace`)
4. ✅ Runs clippy (`cargo clippy --workspace`)
5. ✅ Builds web UI to verify it works
6. ✅ Updates version in Cargo.toml files
7. ✅ Creates/updates CHANGELOG.md
8. ✅ Commits version bump
9. ✅ Creates git tag
10. ✅ Optionally pushes to GitHub

### Example

```bash
$ ./scripts/release.sh 1.0.0

Creating release for version v1.0.0
Running tests...
    Finished test [unoptimized + debuginfo] target(s) in 2.34s
Running clippy...
    Finished dev [unoptimized + debuginfo] target(s) in 0.12s
Checking web build...
✓ Build completed successfully
Updating version in Cargo.toml files...
Version updated in:
  - Cargo.toml
  - crates/temps-cli/Cargo.toml

Please review CHANGELOG.md and update it for this release
Press Enter when ready to continue...

Committing version bump...
Creating tag v1.0.0...
════════════════════════════════════════
Release v1.0.0 prepared!
════════════════════════════════════════

Next steps:
  1. Review the changes:
     git show HEAD

  2. Push to GitHub:
     git push origin main
     git push origin v1.0.0

  3. Monitor the release workflow:
     https://github.com/YOUR_ORG/temps/actions

Push now? (y/N)
```

## Adding More Scripts

When adding new scripts:

1. Make them executable: `chmod +x scripts/your-script.sh`
2. Add shebang: `#!/bin/bash`
3. Use `set -e` to exit on errors
4. Document them in this README
