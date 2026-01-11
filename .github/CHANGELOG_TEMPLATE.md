# Changelog Entry Template

When updating `CHANGELOG.md`, add your changes under the `[Unreleased]` section using one of these categories:

## Categories

### Added
For new features or functionality.

**Examples:**
- Added support for custom domains
- Added PostgreSQL backup scheduling
- Added dark mode to web UI

### Changed
For changes in existing functionality.

**Examples:**
- Changed default deployment timeout from 5m to 10m
- Updated API response format for deployments
- Improved error messages for failed builds

### Deprecated
For soon-to-be removed features.

**Examples:**
- Deprecated `old_api_endpoint` in favor of `new_api_endpoint`
- Deprecated support for Node.js 14

### Removed
For now removed features.

**Examples:**
- Removed deprecated `/v1/old-endpoint` API
- Removed support for legacy deployment format

### Fixed
For any bug fixes.

**Examples:**
- Fixed memory leak in proxy server
- Fixed incorrect timezone handling in analytics
- Fixed deployment failing when branch name contains slashes

### Security
For security-related changes.

**Examples:**
- Fixed SQL injection vulnerability in search endpoint
- Updated dependencies to patch security issues
- Added rate limiting to authentication endpoints

## Format Example

```markdown
## [Unreleased]

### Added
- Support for custom domains with automatic TLS certificates
- PostgreSQL connection pooling configuration
- Dark mode toggle in user settings

### Changed
- Improved deployment progress reporting with detailed stages
- Updated default memory limit for containers from 512MB to 1GB

### Fixed
- Fixed race condition in deployment status updates
- Corrected timezone display in analytics dashboard
- Resolved issue with environment variables not being properly escaped

### Security
- Updated all dependencies to latest secure versions
- Added CSRF protection to state-changing API endpoints
```

## Guidelines

1. **Be specific**: Describe what changed, not just "updated X"
2. **User-focused**: Write from the user's perspective
3. **Actionable**: Help users understand what they need to do (if anything)
4. **Concise**: One line per change, use bullet points
5. **Present tense**: "Add feature" not "Added feature"
6. **Link issues**: Reference issue/PR numbers when relevant

## Good Examples ✅

- Fixed deployment timeout for large Docker images (#123)
- Added support for ARM64 Linux binaries
- Changed default log retention from 7 to 30 days
- Removed experimental feature flag for session replay

## Bad Examples ❌

- Updated code (too vague)
- Fixed stuff (not specific)
- Changes (no description)
- Made it better (subjective, not descriptive)

## When to Skip Changelog

Add the `skip-changelog` label to PRs that don't need a changelog entry:

- Documentation-only changes (typos, clarifications)
- CI/build configuration changes
- Dependency updates with no user-facing impact
- Code refactoring with no functional changes
- Test additions/improvements

## Need Help?

See the [Keep a Changelog](https://keepachangelog.com/) guide for more details.
