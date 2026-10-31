<!-- SPDX-FileCopyrightText: 2024-2026 Temps Contributors -->
<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# Edge rules: block, allow, challenge, redirect or log requests

Edge rules are a project's ordered list of request rules. The Temps proxy
checks them before a request reaches any container: top to bottom, and the
first matching rule that decides (block, allow, challenge, redirect) ends the
evaluation. A request no rule decides goes to the app.

Use them for: blocking scanners on `/wp-admin` or `/.env`, keeping an admin
path to an office network, challenging AI crawlers on content pages,
redirecting an old path, or logging how often a pattern happens before acting
on it.

They run after IP restrictions and before the project's rate limits, Shield
and password protection. Certificate checks (`/.well-known/acme-challenge/`)
and Temps' own endpoints (`/api/_temps/`, the challenge solver files under
`/api/__temps/`, and the password form's `POST /_temps/password-verify` when
the environment has a password) are never evaluated, so no rule can break
HTTPS issuance, the challenge it serves, or unlocking a protected site.

The console page is **Project → Settings → Edge rules**.

## Safe workflow

A rule that matches too much blocks real users the moment it is saved; there
is no draft state. Introduce every rule like this:

1. **Inspect** the existing rules first, read-only:
   `edge-rules list --project-id <id> --json`.
2. **Dry-run** the request you mean to match, and one you must not match:
   `edge-rules test --project-id <id> --url https://app.example.com/wp-admin`.
   `--file rules.json` tests unsaved rules the same way.
3. **Create it in log-only mode** (the default for the API, the CLI and
   `apply`; only `"mode": "enforce"` enforces). It counts matches and never
   acts.
4. **Watch the counter**: `edge-rules list` shows matches per rule. Confirm it
   only matches what was intended.
5. **Enforce** it, with the user's confirmation:
   `edge-rules enforce <rule-id> --project-id <id>`.
6. **Verify** with a real request (`curl -sI https://app.example.com/wp-admin`
   expecting the rule's status) and a request that must still pass.

Agents connected over MCP (networking group, write mode) follow the same
path: `create_edge_rule` always creates log-only rules, and enforcing is a
separate `set_edge_rule_mode` proposal a human confirms with `confirm_action`.

## Rule format

```json
{
  "name": "Block WordPress probes",
  "mode": "log_only",
  "enabled": true,
  "environment_id": null,
  "expression": {
    "combinator": "any",
    "children": [
      { "kind": "condition", "field": "path", "op": "starts_with", "values": ["/wp-admin", "/wp-login.php"], "case_insensitive": true },
      { "kind": "condition", "field": "path", "op": "eq", "values": ["/.env"] }
    ]
  },
  "action": { "type": "block", "status": 404 }
}
```

- **Combinators:** `all`, `any`, `none`. Children are conditions or nested
  groups (`{"kind": "group", "combinator": ..., "children": [...]}`).
- **Fields:** `ip`, `country`, `continent`, `asn`, `is_hosting_provider`,
  `host`, `path` (decoded, normalized), `raw_path`, `method`, `query`,
  `header`, `cookie` (these three need `"key"`), `user_agent`, `ja4`,
  `bot_class` (`human`, `search_engine`, `ai_crawler`, `ai_agent`,
  `unknown_bot`).
- **Operators:** `eq`, `in`, `in_cidr`, `starts_with`, `ends_with`,
  `contains`, `glob`, `regex`, `exists`, `not_exists`.
- **Paths ignore case by default** in the console; set `"case_insensitive":
  true` in files to match it.
- **Actions:** `block` (`status` 400–499, optional `body`), `allow` (skip
  the remaining edge rules), `challenge` (Shield's proof-of-work page; API
  clients get 429), `redirect` (`target`, `status` 301/302/307/308; `{path}`
  and `{query}` insert the request's), `log`.
- `environment_id: null` applies to every environment of the project.

The server validates every rule and returns every problem at once. Limits:
100 rules per project, 32 conditions per rule, and per project at most 32
`regex` conditions and 256 pattern conditions (every `glob` and `regex`, and
`starts_with`, `ends_with` or `contains` ignoring case, which paths do by
default). Prefer one `in` condition over many `eq`/`starts_with` ones where
it fits; it is not counted. `asn` values are AS numbers,
with or without the `AS` prefix (`AS64496`, `64496`).

## Manage rules as a file

```bash
edge-rules export --project-id <id> --output edge-rules.json
# edit, review, commit
edge-rules apply --project-id <id> --file edge-rules.json --dry-run
edge-rules apply --project-id <id> --file edge-rules.json
```

`apply` matches rules by name: changed rules are updated in place (keeping
their ids and counters), missing ones created, rules absent from the file
deleted, then everything is ordered as in the file. A rule without `mode`
is log-only, and the plan marks each created or updated rule `[enforce]` or
`[log only]`. Always show the
`--dry-run` plan to the user before applying; a deletion or an enforced rule
takes effect within seconds.

## Permissions

`edge_rules:read` lists, reads and dry-runs; `edge_rules:write` changes
rules. Admins and users have both; readers have read. Every change is in the
audit log with the rule before and after.

## Version check

`edge-rules` is newer than some reviewed CLI versions. Confirm
`edge-rules --help` exists in the pinned runtime before relying on it; if it
does not, use the console page and report the version gap.
