## Pass summary — run 2026-05-27T08:16Z

Added `export const sections` and `{{ anchor: true, id: '...' }}` anchor IDs to all H2 headings in
`docs/features/session-replay/page.mdx`, which was missing the sections export and anchor hooks
present on all other feature pages. Also rewrote the lead paragraph (it duplicated the metadata
description verbatim) and tightened the Overview intro which repeated the same sentence a third
time. Updated the stale version example in `docs/howto/upgrade-temps/page.mdx` from
`0.1.0-beta.21` (pass 1 value) to `0.1.0-beta.22` (current release per CHANGELOG).
Merged origin/main (preview-on-demand / cancel comment / build limits release).

### Risk
REVIEW

### Files changed
- `docs/features/session-replay/page.mdx` — added sections export, anchor IDs on all H2s, rewrote lead and overview to remove triple repetition of the same sentence
- `docs/howto/upgrade-temps/page.mdx` — stale `0.1.0-beta.21` example → `0.1.0-beta.22`

### Stub filled
none this pass

### Clarity rewrite
`session-replay` — added sections/anchor-id scaffolding present on all peer pages; removed triple-repeated lead sentence; tightened overview paragraph

### Stale refs fixed
- `0.1.0-beta.21 (abc1234) built 2026-03-04` → `0.1.0-beta.22 (abc1234) built 2026-05-26` (in `docs/howto/upgrade-temps/page.mdx`)
