# Docs Auto-Improve Log

## Pass 1 — run 20260526-102603

**Date:** 2026-05-26
**Risk:** REVIEW

### Changes

| File | Change |
|------|--------|
| `docs/howto/upgrade-temps/page.mdx` | Stale version string `v0.0.6` → `0.1.0-beta.21` |
| `docs/migrate/from-heroku/page.mdx` | Stub filled: complete Heroku migration guide (concept mapping, Procfile, database export/import, env vars, DNS cutover, add-on replacements, rollback plan) |
| `docs/features/monitoring/page.mdx` | Clarity pass: rewrote Understanding Metrics, Monitoring Best Practices, and Troubleshooting Property blocks — multi-point content was inline run-on text, now properly bulleted with actionable thresholds |
| `docs/introduction/page.mdx` | Fixed cramped Row/Col "Why Temps?" bullet lists |
| `docs/features/logs/page.mdx` | Fixed cramped Row/Col log-type lists in Types of Logs section |
| `docs/features/skills/page.mdx` | Fixed cramped Row/Col bullet list in Overview section |
| `docs/features/mcp/page.mdx` | Fixed cramped Row/Col bullet list in Overview; corrected "rollback" to "roll back" (verb) |

### Already done (do not repeat)
- `from-heroku` migration stub — filled in this pass
- Monitoring page Property blocks — rewritten in this pass
- Stale version `v0.0.6` in upgrade-temps page — fixed in this pass

## Pass 2 — run 20260526-122641

**Date:** 2026-05-26
**Risk:** REVIEW

### Changes

| File | Change |
|------|--------|
| `docs/features/attack-mode/page.mdx` | Fixed 29 prose `--` → `—` (em dash) |
| `docs/features/kv-storage/page.mdx` | Fixed 2 prose `--` → `—` |
| `docs/features/skills/page.mdx` | Fixed 3 prose `--` → `—` |
| `docs/features/webhooks/page.mdx` | Clarity pass: sections export, lead paragraph, anchor headings, Testing section reorganisation, fixed contraction |
| `docs/howto/set-up-managed-services/page.mdx` | Stale RustFS image: `1.0.0-alpha.78` → `1.0.0-alpha.98` |
| `docs/migrate/from-railway/page.mdx` | Stub filled: complete Railway migration guide |
| `docs/tutorials/deploy-with-database/page.mdx` | Stale RustFS image: `1.0.0-alpha.78` → `1.0.0-alpha.98` |

### Already done (do not repeat)
- `from-heroku` migration stub — filled in pass 1
- Monitoring page Property blocks — rewritten in pass 1
- Stale version `v0.0.6` in upgrade-temps page — fixed in pass 1
- `from-railway` migration stub — filled in pass 2
- Webhooks page structure — rewritten in pass 2
- Stale RustFS `1.0.0-alpha.78` in set-up-managed-services and deploy-with-database — fixed in pass 2
- Double-dash `--` em-dash typos in attack-mode, kv-storage, skills — fixed in pass 2

## Pass 3 — run 20260526-143000

**Date:** 2026-05-26
**Risk:** REVIEW

### Changes

| File | Change |
|------|--------|
| `docs/migrate/from-vercel/page.mdx` | Stub filled: complete Vercel migration guide (concept mapping, env-var export and translation, DNS cutover, Next.js notes, feature replacements, rollback plan) |

### Already done (do not repeat)
- `from-heroku` migration stub — filled in pass 1
- Monitoring page Property blocks — rewritten in pass 1
- Stale version `v0.0.6` in upgrade-temps page — fixed in pass 1
- `from-railway` migration stub — filled in pass 2
- Webhooks page structure — rewritten in pass 2
- Stale RustFS `1.0.0-alpha.78` in set-up-managed-services and deploy-with-database — fixed in pass 2
- Double-dash `--` em-dash typos in attack-mode, kv-storage, skills — fixed in pass 2
- `from-vercel` migration stub — filled in pass 3

## Pass 4 — run 20260526-162642

**Date:** 2026-05-26
**Risk:** REVIEW

### Changes

| File | Change |
|------|--------|
| `docs/migrate/from-netlify/page.mdx` | Stub filled: complete Netlify migration guide (concept mapping, env-var translation, netlify.toml handling, Functions→container migration, DNS cutover, feature replacements, rollback plan) |
| `docs/architecture/overview/page.mdx` | Clarity pass: added `{{ className: 'lead' }}` lead paragraph, replaced vague description with architecture-accurate summary |

### Already done (do not repeat)
- `from-heroku` migration stub — filled in pass 1
- Monitoring page Property blocks — rewritten in pass 1
- Stale version `v0.0.6` in upgrade-temps page — fixed in pass 1
- `from-railway` migration stub — filled in pass 2
- Webhooks page structure — rewritten in pass 2
- Stale RustFS `1.0.0-alpha.78` in set-up-managed-services and deploy-with-database — fixed in pass 2
- Double-dash `--` em-dash typos in attack-mode, kv-storage, skills — fixed in pass 2
- `from-vercel` migration stub — filled in pass 3
- `from-netlify` migration stub — filled in pass 4
- `architecture/overview` lead paragraph and description — rewritten in pass 4

## Pass 5 — run 20260526-181600

**Date:** 2026-05-26
**Risk:** REVIEW

### Changes

| File | Change |
|------|--------|
| `docs/reference/troubleshooting/page.mdx` | Stub filled: 18 concrete failure scenarios across build failures, health check failures, runtime errors, SSL/domain issues, env-var problems, database connection errors, cron job failures, performance problems, and CLI/API errors |
| `docs/advanced/performance/page.mdx` | Stub filled: end-to-end performance optimization guide covering resource sizing, build layer caching, multi-stage Dockerfiles, HTTP/memory/Redis caching, database query profiling and indexing, horizontal scaling prerequisites, CDN/static-asset offloading, and OpenTelemetry tracing setup |

### Already done (do not repeat)
- `from-heroku` migration stub — filled in pass 1
- Monitoring page Property blocks — rewritten in pass 1
- Stale version `v0.0.6` in upgrade-temps page — fixed in pass 1
- `from-railway` migration stub — filled in pass 2
- Webhooks page structure — rewritten in pass 2
- Stale RustFS `1.0.0-alpha.78` in set-up-managed-services and deploy-with-database — fixed in pass 2
- Double-dash `--` em-dash typos in attack-mode, kv-storage, skills — fixed in pass 2
- `from-vercel` migration stub — filled in pass 3
- `from-netlify` migration stub — filled in pass 4
- `architecture/overview` lead paragraph and description — rewritten in pass 4
- `reference/troubleshooting` stub — filled in pass 5
- `advanced/performance` stub — filled in pass 5

## Pass 6 — run 20260526-200000

**Date:** 2026-05-26
**Risk:** REVIEW

### Changes

| File | Change |
|------|--------|
| `docs/features/teams/page.mdx` | Stub filled: complete Teams & Collaboration guide sourced from codebase permissions.rs, auth schema, and audit_logs entity |
| `docs/architecture/security/page.mdx` | Clarity rewrite: corrected role names and permission string format, added lead paragraph and section anchors, rewrote informal prose, replaced deprecated whitelist/blacklist terminology |

### Already done (do not repeat)
- `from-heroku` migration stub — filled in pass 1
- Monitoring page Property blocks — rewritten in pass 1
- Stale version `v0.0.6` in upgrade-temps page — fixed in pass 1
- `from-railway` migration stub — filled in pass 2
- Webhooks page structure — rewritten in pass 2
- Stale RustFS `1.0.0-alpha.78` in set-up-managed-services and deploy-with-database — fixed in pass 2
- Double-dash `--` em-dash typos in attack-mode, kv-storage, skills — fixed in pass 2
- `from-vercel` migration stub — filled in pass 3
- `from-netlify` migration stub — filled in pass 4
- `architecture/overview` lead paragraph and description — rewritten in pass 4
- `reference/troubleshooting` stub — filled in pass 5
- `advanced/performance` stub — filled in pass 5
- `features/teams` stub — filled in pass 6
- `architecture/security` role names, permission strings, informal prose — rewritten in pass 6

## Pass 7 — run 20260526-221600

**Date:** 2026-05-26
**Risk:** REVIEW

### Changes

| File | Change |
|------|--------|
| `docs/advanced/custom-buildpacks/page.mdx` | Stub filled: complete guide covering Nixpacks auto-detection, `.nixpacks.toml` configuration, custom Dockerfiles, multi-stage builds, layer caching, monorepo setup, and dashboard build-setting overrides |

### Already done (do not repeat)
- `from-heroku` migration stub — filled in pass 1
- Monitoring page Property blocks — rewritten in pass 1
- Stale version `v0.0.6` in upgrade-temps page — fixed in pass 1
- `from-railway` migration stub — filled in pass 2
- Webhooks page structure — rewritten in pass 2
- Stale RustFS `1.0.0-alpha.78` in set-up-managed-services and deploy-with-database — fixed in pass 2
- Double-dash `--` em-dash typos in attack-mode, kv-storage, skills — fixed in pass 2
- `from-vercel` migration stub — filled in pass 3
- `from-netlify` migration stub — filled in pass 4
- `architecture/overview` lead paragraph and description — rewritten in pass 4
- `reference/troubleshooting` stub — filled in pass 5
- `advanced/performance` stub — filled in pass 5
- `features/teams` stub — filled in pass 6
- `architecture/security` role names, permission strings, informal prose — rewritten in pass 6
- `advanced/custom-buildpacks` stub — filled in pass 7

---

## Pass 8 — run-2026-05-27-001

**Date:** 2026-05-27
**Risk:** REVIEW

### Changes

| File | Change |
|------|--------|
| `docs/advanced/networking/page.mdx` | Stub filled: complete networking guide (ports, internal networking, split-listener proxy, IP ACLs, WireGuard tunnels, load balancing, host firewall rules) |
| `docs/architecture/data-flow/page.mdx` | Clarity pass: added sections export, lead paragraph, `---` dividers, lowercase anchor headings matching site conventions |
| `docs/features/cron-jobs/page.mdx` | Grammar fix: removed "Competitive Advantage" marketing label from Note block |
| `docs/features/managed-services/page.mdx` | Grammar fix: removed "Competitive Advantage" marketing label from Note block |

### Already done (do not repeat)
- `from-heroku` migration stub — filled in pass 1
- Monitoring page Property blocks — rewritten in pass 1
- Stale version `v0.0.6` in upgrade-temps page — fixed in pass 1
- `from-railway` migration stub — filled in pass 2
- Webhooks page structure — rewritten in pass 2
- Stale RustFS `1.0.0-alpha.78` in set-up-managed-services and deploy-with-database — fixed in pass 2
- Double-dash `--` em-dash typos in attack-mode, kv-storage, skills — fixed in pass 2
- `from-vercel` migration stub — filled in pass 3
- `from-netlify` migration stub — filled in pass 4
- `architecture/overview` lead paragraph and description — rewritten in pass 4
- `reference/troubleshooting` stub — filled in pass 5
- `advanced/performance` stub — filled in pass 5
- `features/teams` stub — filled in pass 6
- `architecture/security` role names, permission strings, informal prose — rewritten in pass 6
- `advanced/custom-buildpacks` stub — filled in pass 7
- `advanced/networking` stub — filled in pass 8
- `architecture/data-flow` sections export and lead paragraph — rewritten in pass 8
- "Competitive Advantage" Note blocks in cron-jobs and managed-services — fixed in pass 8
