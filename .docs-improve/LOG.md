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
