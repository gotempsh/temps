## Pass summary — run 20260526-102603

This pass fixed a stale version string in the upgrade how-to guide (v0.0.6 → 0.1.0-beta.21), filled the Heroku migration stub with a complete step-by-step guide covering concept mapping, Procfile handling, database migration, config-var import, DNS cutover, add-on replacements, and a rollback plan, rewrote the monitoring page's cramped Property blocks so each bullet appears on its own line with specific actionable thresholds, and fixed four more pages with identical Row/Col rendering issues (logs, skills, mcp, introduction).

### Risk
REVIEW

### Files changed
- `docs/howto/upgrade-temps/page.mdx` — stale version string: `v0.0.6` → `0.1.0-beta.21`
- `docs/migrate/from-heroku/page.mdx` — stub filled with complete Heroku migration guide
- `docs/features/monitoring/page.mdx` — clarity pass: rewrote cramped Property blocks
- `docs/introduction/page.mdx` — fixed cramped Row/Col bullet list in "Why Temps?" section
- `docs/features/logs/page.mdx` — fixed cramped Row/Col log-type lists
- `docs/features/skills/page.mdx` — fixed cramped Row/Col bullet list in Overview
- `docs/features/mcp/page.mdx` — fixed cramped Row/Col bullet list; corrected "rollback" to "roll back" (verb form)

### Stub filled
`from-heroku` — complete step-by-step Heroku migration guide.

### Clarity rewrite
`docs/features/monitoring/page.mdx` — rewrote all three Properties blocks where multi-point content was collapsed into single-line run-on text.

### Stale refs fixed
- `v0.0.6` → `0.1.0-beta.21` (in `docs/howto/upgrade-temps/page.mdx`)
