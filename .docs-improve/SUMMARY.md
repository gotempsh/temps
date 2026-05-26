## Pass summary — run 20260526-162642

Filled the `from-netlify` migration stub with a complete guide matching the style of `from-vercel` and `from-railway`: concept mapping table, five migration steps (env-var export, app prep including Functions→containers translation, project creation, env-var import with Netlify platform variable translation table, DNS cutover), feature replacement table, and rollback plan. Performed a clarity pass on `architecture/overview`: replaced a vague marketing description and missing lead paragraph with an accurate architecture-focused summary, and updated the metadata description to match the page title.

### Risk
REVIEW

### Files changed
- `docs/migrate/from-netlify/page.mdx` — stub filled: complete Netlify migration guide
- `docs/architecture/overview/page.mdx` — clarity pass: added lead paragraph, replaced vague description with architecture-accurate summary

### Stub filled
from-netlify — step-by-step guide to migrating from Netlify covering concept mapping, env-var translation, netlify.toml handling, Functions→container migration, DNS cutover, feature replacements, and rollback plan

### Clarity rewrite
architecture/overview — replaced a missing lead paragraph and a description ("Learn what Temps can do for you") that did not match the page title ("Architecture Overview") with an accurate single-binary architecture summary and a proper lead paragraph

### Stale refs fixed
none this pass
