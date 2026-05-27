## Pass summary — run 2026-05-27T10:00:00Z

This pass completed a grammar and clarity sweep across nine MDX files. The `architecture/deployment` page received a full clarity rewrite: `export const sections` and `{{ anchor: true, id }}` attributes were added to all H2 headings, the lead paragraph was updated to match the site's standard `{{ className: 'lead' }}` pattern, `---` section dividers were inserted, and all informal contractions and style inconsistencies (dashes as hyphens, CamelCase bullets, "Here's", "don't", "you'll") were corrected. The `advanced/security` page had deprecated `whitelist` terminology replaced with `allowlist` in prose and CLI comments. Contractions were eliminated from seven additional pages: `introduction`, `upgrade`, `features/backups`, `features/error-tracking`, `howto/admin-listener`, `howto/enable-clickhouse-analytics`, and `howto/cli-login`.

### Risk
REVIEW

### Files changed
- `docs/architecture/deployment/page.mdx` — clarity rewrite: added sections export, lead paragraph, anchor IDs, section dividers, em dashes, formal prose
- `docs/advanced/security/page.mdx` — replaced deprecated "whitelist" terminology with "allowlist" in prose and CLI comments
- `docs/features/backups/page.mdx` — fixed contractions ("If you're" → "If you are", "Doesn't" → "does not")
- `docs/features/error-tracking/page.mdx` — fixed contractions ("you'll find" → "you will find", "don't want" → "do not want")
- `docs/howto/admin-listener/page.mdx` — fixed contractions across six prose lines
- `docs/upgrade/page.mdx` — fixed contractions ("it's" → "it is", "that's" → "that has", "isn't" → "is not")
- `docs/howto/enable-clickhouse-analytics/page.mdx` — fixed contractions ("don't want", "don't need", "isn't running", "doesn't speak")
- `docs/howto/cli-login/page.mdx` — fixed contractions ("aren't", "don't act", "doesn't need", "can't sit")
- `docs/introduction/page.mdx` — fixed contractions ("you're ready", "don't have", "it's running", "it's likely")

### Stub filled
none this pass

### Clarity rewrite
`architecture/deployment` — added sections export, lead paragraph, anchor IDs, section dividers, em dashes, formal prose

### Stale refs fixed
none this pass
