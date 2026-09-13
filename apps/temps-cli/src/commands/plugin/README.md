# Plugin publication contracts

Bun installs development dependencies, runs tests, and cross-compiles native
plugins. `npm publish` is intentionally used for npm's interactive browser/2FA
publication flow; it runs with lifecycle scripts disabled. The Cursor rule in
`sdks/node/examples/kv/kv-simple/.cursor/rules/` is scoped to that example, not
this CLI. Do not replace npm's approval flow with a bypass token.

## Crash recovery API dependency

Before the first remote create, the CLI atomically saves `request.json` with
the metadata digest and a random 256-bit `recoveryToken`. Treat this journal
as private (mode 0600); do not commit or share it. Preserve `.temps-plugin`
until publication completes.

`POST /api/plugin-publisher` must accept the optional `recoveryToken` on
`operation: "create"`. For the authenticated account and exact same metadata,
replaying that token must return the original draft ID, expiry, package IDs,
and verification codes. It must not consume another draft quota or rotate
challenges. Different tokens or metadata must fail for an existing release.
Cross-account retries must never retrieve another publisher's draft.

This requires the companion publisher API update before this CLI is released.
Older strict APIs reject the extra field, safely stopping before npm publication.
Do not retry with the field removed: that would restore the orphaned-draft window.

The CLI syncs a temporary file before atomic rename, then syncs the containing
directory. A failed save cannot truncate the previous state. Invalid JSON,
invalid timestamps, unknown statuses, or mismatched/duplicate package IDs stop
publication with a contextual error rather than silently creating another draft.

Existing valid `release.json` files continue to resume without a create call.
Legacy orphaned drafts with no recovery journal cannot be recovered by this
protocol and require maintainer intervention or a new version.
