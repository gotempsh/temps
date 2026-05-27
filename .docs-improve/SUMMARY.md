## Pass summary — run 2026-05-27T02:16:00Z

Filled the last remaining stub, `docs/features/api/page.mdx`, with a seven-section overview covering API authentication (Bearer tokens, the auto-injected `TEMPS_API_TOKEN`, and where to create keys), a capabilities table for every resource category, a deployment-trigger curl example, deployment-status polling with job-log retrieval, environment variable management, artifact-upload endpoints (registry image, Docker tarball, static bundle), and CLI/Node SDK quick-starts. Also applied a clarity pass to `docs/features/rollbacks/page.mdx` — the page was missing `export const sections`, the `{{ className: 'lead' }}` intro class, `---` section dividers, and `{{ anchor: true, id }}` attributes on all H2 headers; fixed the broken internal link `/docs/deployment` → `/docs/deployments`; and updated the stale `2024-01-15` example dates to `2026-05-24`.

### Risk
REVIEW

### Files changed
- `docs/features/api/page.mdx` — stub filled: API overview (auth, capabilities, deploy trigger, status polling, env-var management, artifact upload, CLI/SDK)
- `docs/features/rollbacks/page.mdx` — clarity pass: sections export, lead class, `---` dividers, anchors; broken link `/docs/deployment` → `/docs/deployments`; updated stale example dates

### Stub filled
features/api — seven-section guide enabling programmatic control: authentication, capabilities overview, trigger-deployment example, deployment-status polling, env-var CRUD, artifact-upload endpoints, and CLI/SDK quick-starts

### Clarity rewrite
features/rollbacks — added `export const sections`, `{{ className: 'lead' }}`, `---` dividers, and `{{ anchor: true, id }}` on all H2 headers to match the established page structure; the page was missing all navigation scaffolding

### Stale refs fixed
- `2024-01-15` → `2026-05-24` in example `temps deployments list` output (in `docs/features/rollbacks/page.mdx`)
