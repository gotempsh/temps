## Pass summary — run 20260526-221600

Filled the `advanced/custom-buildpacks` stub with a complete guide covering the Nixpacks auto-detection priority order, `.nixpacks.toml` configuration (build/start command override, runtime version pins, system packages, multi-step builds), Procfile compatibility, custom Dockerfiles and when to prefer them over Nixpacks, multi-stage build patterns with Node.js and Rust examples, Docker layer caching strategies, monorepo app-directory configuration, and per-project build-settings overrides from the dashboard. Content sourced from `docs/reference/supported-frameworks/page.mdx`, `docs/reference/troubleshooting/page.mdx`, and `docs/migrate/from-{heroku,railway}/page.mdx`. No typos, broken links, or stale version strings were found this pass.

### Risk
REVIEW

### Files changed
- `docs/advanced/custom-buildpacks/page.mdx` — Stub replaced with complete Nixpacks/Dockerfile/multi-stage/caching guide

### Stub filled
advanced/custom-buildpacks — end-to-end guide for customizing the build pipeline with `.nixpacks.toml`, custom Dockerfiles, multi-stage builds, and layer caching best practices

### Clarity rewrite
none this pass

### Stale refs fixed
none this pass
