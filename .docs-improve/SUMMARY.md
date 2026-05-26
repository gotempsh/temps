## Pass summary — run 20260526-122641

This pass fixed `--` (double-dash) prose em-dash typos in four pages (attack-mode, kv-storage, skills, webhooks) where the rest of the docs corpus consistently uses the `—` Unicode em dash, updated a stale RustFS Docker image tag from `1.0.0-alpha.78` to `1.0.0-alpha.98` (matching the constant in `crates/temps-blob/src/services/config.rs`) in two doc pages, filled the Railway migration stub with a complete ten-section guide covering concept mapping, database export and restore, environment-variable translation, DNS cutover, and a rollback plan, and applied a clarity pass to the webhooks page by adding a `sections` export, a lead paragraph following the standard format, `{{ anchor: true, id: '...' }}` on all h2 headings, reorganising the confusing "Error Messages" subsection into "Delivery errors" with clear cause-and-fix framing, and removing the contraction `you'll`.

### Risk
REVIEW

### Files changed
- `docs/features/attack-mode/page.mdx` — replaced 29 prose `--` instances with `—` em dash for consistency with the rest of the corpus
- `docs/features/kv-storage/page.mdx` — replaced 2 prose `--` instances with `—`
- `docs/features/skills/page.mdx` — replaced 3 prose `--` instances with `—`
- `docs/features/webhooks/page.mdx` — clarity pass: added sections export, lead paragraph, anchor headings, reorganised Testing section, fixed contraction
- `docs/howto/set-up-managed-services/page.mdx` — stale RustFS image tag `1.0.0-alpha.78` → `1.0.0-alpha.98`
- `docs/migrate/from-railway/page.mdx` — stub filled with complete Railway migration guide
- `docs/tutorials/deploy-with-database/page.mdx` — stale RustFS image tag `1.0.0-alpha.78` → `1.0.0-alpha.98`

### Stub filled
`from-railway` — complete step-by-step Railway-to-Temps migration guide covering concept mapping, database export/restore, env-var translation, DNS cutover, service replacements, and rollback plan.

### Clarity rewrite
`docs/features/webhooks/page.mdx` — added `sections` export and `{{ anchor: true }}` headings, rewrote the intro paragraph to follow the standard lead format, reorganised the "Testing Webhooks → Error Messages" section into a logical "Testing → Delivery errors" structure with cause-and-fix framing, fixed contraction `you'll`.

### Stale refs fixed
- `rustfs/rustfs:1.0.0-alpha.78` → `rustfs/rustfs:1.0.0-alpha.98` (in `docs/howto/set-up-managed-services/page.mdx`)
- `rustfs/rustfs:1.0.0-alpha.78` → `rustfs/rustfs:1.0.0-alpha.98` (in `docs/tutorials/deploy-with-database/page.mdx`)
