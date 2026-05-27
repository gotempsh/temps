## Pass summary — run 2026-05-27-run

This pass completed a terminology-consistency sweep replacing deprecated whitelist/blocklist/allowlist language in prose across several pages that were not covered by pass 10, fixed contractions in prose sections of `features/cron-jobs` and `architecture/plugins`, and replaced informal first-person constructions ("We recommend", "Here's") with formal alternatives in `errors`, `features/analytics`, `features/mcp`, and `reference/cli-getting-started`.

### Risk
SAFE

### Files changed
- `docs/features/attack-mode/page.mdx` — "Block List"/"Allow List" headings → "Blocklist"/"Allowlist"; Note "whitelist and blacklist IPs" → "allowlist and blocklist IPs"; table rows updated
- `docs/architecture/request-flow/page.mdx` — "Block List"/"Allow List" bullets → "Blocklist"/"Allowlist"
- `docs/architecture/overview/page.mdx` — "IP Whitelisting" → "IP Allowlisting"
- `docs/features/cron-jobs/page.mdx` — four prose contractions expanded ("don't" → "do not", "doesn't" → "does not")
- `docs/architecture/plugins/page.mdx` — prose contraction "it's live" → "it is live"
- `docs/errors/page.mdx` — "We recommend handling errors" → "Handle errors" (removed first-person plural)
- `docs/features/analytics/page.mdx` — "Here's a complete example" → formal phrasing
- `docs/features/mcp/page.mdx` — "Here's a summary" → "The following table summarises"
- `docs/reference/cli-getting-started/page.mdx` — "Here's a quick workflow" → formal phrasing

### Stub filled
none this pass

### Clarity rewrite
none this pass

### Stale refs fixed
- "Block List" → "Blocklist" (in `docs/features/attack-mode/page.mdx`, `docs/architecture/request-flow/page.mdx`)
- "Allow List" → "Allowlist" (in `docs/features/attack-mode/page.mdx`, `docs/architecture/request-flow/page.mdx`)
- "IP Whitelisting" → "IP Allowlisting" (in `docs/architecture/overview/page.mdx`)
- "whitelist and blacklist IPs" → "allowlist and blocklist IPs" (in `docs/features/attack-mode/page.mdx`)
