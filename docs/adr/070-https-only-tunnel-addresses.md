# ADR 070: HTTPS only, on standard ports, for addresses published through a Cloudflare tunnel

## Status

Proposed — 2026-10-28

Related: ADR 058 (ingress modes, Cloudflare Tunnel, D7 reachability check),
ADR 064 (making a project reachable: D4 plan, run journal and undo, D7 checks,
D8 the Cloudflare token), ADR 065 (D9 Cloudflare Access, D10 checking what
answers at an address), ADR 068 (environment hosts under a tunnel's domain).

## Context

*Make it reachable* publishes a hostname through a Cloudflare tunnel with a
proxied CNAME to `<tunnel>.cfargotunnel.com` (ADR 058). Cloudflare's edge then
answers that hostname on every port it proxies, not just 443:

- **Plain HTTP.** `http://<host>/` on port 80 is proxied to the tunnel and the
  app answers `200`. The visitor's request and the app's answer cross the
  internet unencrypted as far as Cloudflare. Nothing in Temps redirects it:
  the worker's tunnel origin listener is told every request is HTTPS
  (`spawn_tunnel_origin` builds its router with `forwarded_proto = "https"`)
  and never looks at the `X-Forwarded-Proto: http` Cloudflare sends.
- **Alternate edge ports.** Cloudflare also proxies HTTP on 8080, 8880, 2052,
  2082, 2086, 2095 and HTTPS on 8443, 2053, 2083, 2087, 2096. A tunneled
  hostname answers on all of them. The user chose none of them, and a scanner
  that finds the app on `:8080` sees it over plain HTTP again.

Temps sets up no redirect or port restriction today: there is no handling of
`always_use_https` or `cf.edge.server_port` anywhere. A user who reviews the
plan sees "Cloudflare serves HTTPS" and reasonably believes that is all it
serves.

Cloudflare's zone setting **Always Use HTTPS** would fix the first half, but
it is zone-wide: turning it on changes every other site in the user's zone,
including ones Temps knows nothing about. ADR 064's rule is that a run only
touches what it records and undo removes exactly that. A zone setting that
other sites rely on can be neither attributed to a run nor safely turned off
by its undo.

## Decision

### D1. A per-address choice, HTTPS only by default

Some users deliberately serve an app over plain HTTP, so nothing here is
forced. Every option that publishes the address through a Cloudflare tunnel
(`existing_tunnel`, `new_tunnel`, `new_tunnel_with_token`, including the
signed-in Access variants of those) asks one question, the optional
`http_policy` field, "What it answers":

| Value | Label | Rules the run creates |
|---|---|---|
| `https_only` | **HTTPS only (recommended)** | the redirect and the port rule |
| `allow_http` | Also plain HTTP on port 80 | the port rule only |
| `allow_all` | Also plain HTTP, and Cloudflare's other ports | none |

- **The default** — what the field's placeholder shows and what applying
  without an answer takes — is the address's recorded choice, else
  `https_only`. A new tunnel address is HTTPS only unless the user turns it
  off before applying.
- **With HTTPS only off**, the plan's steps lose the rules it does not want
  and its warnings say plainly what the address will also answer: "will also
  answer over unencrypted HTTP (http://…)", plus "and on Cloudflare's other
  ports (such as 8080 and 8443)" for `allow_all`. No rule is created or
  journaled for a choice that does not want it.
- **One choice with three values, not two switches.** The fourth
  combination — redirect plain HTTP, but keep 8080/8443 open — is not
  offered: the redirect matches `not ssl`, so it already covers the
  alternate HTTP ports, and leaving the alternate HTTPS ports open serves
  nobody who wanted the redirect. "Plain HTTP on 80 but not on 8080/8443" is
  `allow_http`. Three named outcomes read more plainly in a plan than two
  switches whose combinations the user has to work out.
- **The choice is stored on the address**: a nullable
  `project_custom_domains.http_policy` column (`NULL` = never chosen), set by
  the run's `set_http_policy` step right after the address is attached. A
  re-run reads it back as the default, so it keeps the choice.
- **Never retroactive.** No migration, check or background job sets the
  column or creates a rule. An existing address keeps answering exactly as
  it does today until the user runs *Make it reachable* again and applies a
  plan; a re-run that keeps the recorded choice records nothing new.
- **Choosing less later removes nothing.** A re-run that answers
  `allow_http` or `allow_all` for an address an earlier run made HTTPS only
  creates no rule and deletes none: the earlier run's rules are that run's,
  and undoing it removes them. The field's help says so.
- A hostname more than one label below its zone is never offered
  `https_only` (see below); its default is `allow_http`.

The applied option's steps are rebuilt from the chosen value
(`place_http_policy_steps`), so the plan shown is the default's steps and the
run is the answer's. An answer the option does not offer is refused before
anything starts.

### D1a. The two rules, scoped to the hostname

The rules the choice wants are placed before the address is attached, so the
hostname is never published without them when they can be created:

| Step | Cloudflare phase | Expression | Action |
|---|---|---|---|
| `redirect_to_https` | `http_request_dynamic_redirect` (Single Redirects) | `(http.host eq "<host>" and not ssl)` | `redirect`, 308 to `concat("https://", http.host, http.request.uri.path)`, query string kept |
| `standard_ports_only` | `http_request_firewall_custom` (WAF custom rules) | `(http.host eq "<host>" and not cf.edge.server_port in {80 443})` | `block` |

- **Scoped to the hostname**, never the zone. Other hostnames in the zone, and
  every rule the user already has, are left as they are.
- `not ssl` rather than port 80 only, so plain HTTP on the alternate HTTP
  ports redirects too when the WAF rule could not be created.
- 308 rather than 301 keeps the method of a `POST` sent to `http://`.
- Both rules are available on Cloudflare's free plan (10 Single Redirects and
  5 WAF custom rules per zone).
- **Rules are only ever added, never replaced.** The run reads the phase's
  entry point ruleset; when it exists the rule is added with
  `POST /zones/{zone}/rulesets/{ruleset}/rules`, which leaves the other rules
  as they are. When there is none, the entry point is created with
  `POST /zones/{zone}/rulesets` (`kind: zone`, the phase), which fails rather
  than overwrites if another tool created it in between; the run then reads it
  again and adds the rule. `PUT` on the entry point, which replaces every rule
  in it, is never used.
- **A rule with the same expression and action already there** (the user's,
  or an earlier run's) is used as found: the step is skipped, nothing is
  journaled, and undo never touches it.
- **The hostname is checked before it reaches an expression**: lowercase
  letters, digits, `-` and `.` only, so an address can never inject into the
  rule language. Anything else is refused with a message.
- **A hostname more than one label below its zone** (`a.b.example.com`) gets
  no redirect: Cloudflare's free certificate covers `*.example.com` only, so
  HTTPS on it needs Advanced Certificate Manager, and redirecting to an HTTPS
  address that fails would take the site down. The plan does not offer
  `https_only` for it, says why, and still offers the port rule.
- ADR 068's environment hosts (`tunnel_environment_hosts`) are not covered:
  their hostnames come and go with environments, and a rule per host would
  have to follow them. The check (D4) still notes each one that answers plain
  HTTP. A per-tunnel rule (`ends_with(http.host, ".<tunnel domain>")`) is the
  likely follow-up.

### D2. In the plan, journaled, undone exactly

- The plan shows the steps the chosen value wants with what they create
  ("Redirect `http://myapp.example.com` to HTTPS, with a Cloudflare redirect
  rule for this hostname only"; "Refuse `myapp.example.com` on Cloudflare's
  other ports (8080, 8443, …) with a WAF rule for this hostname only"), and
  one warning naming the token permissions they need and what happens without
  them (D3).
- Recording the choice is its own journal entry (`Intent::HttpPolicy
  {hostname, before, after}`, key `http_policy/<host>`), with the value read
  just before. Undo puts `before` back only while the address still holds
  `after`; a choice made by a later run stays. When the undo also detaches
  the address, the choice goes with it. A re-run whose answer the address
  already holds skips the step and journals nothing.
- Each rule is an ADR 064 D4 journal entry (`Intent::EdgeRule {hostname,
  purpose}`, key `edge_rule/<host>/<purpose>`), written `pending` before the
  call. The rule carries a `ref` and a description that name this install and
  run (`temps-<install>-run<id>-https`), which is how a pending entry is found
  again. Cloudflare documents no read-after-write guarantee for rulesets, so
  the lookup follows the two-miss rule (30 s timeout + 30 s settle), like DNS
  records and Access apps.
- The journaled resource (`RunResource::EdgeRule`) records the DNS provider,
  zone id, ruleset id, rule id and the expression it wrote.
- **Undo** reads the rule by id: gone is done; still holding the expression
  and action the run wrote, it is deleted with
  `DELETE /zones/{zone}/rulesets/{ruleset}/rules/{rule}`; changed since, undo
  pauses on it as a conflict (keep it, or delete it anyway), like an Access
  app whose policies were edited (ADR 065 D9). An entry point ruleset the run
  created is left even when empty: another tool may have added rules to it,
  and an empty one has no effect.

### D3. A token without the permissions publishes anyway, and says why not

The rules need two permissions the ADR 064 D8 token does not ask for:

- **Zone · Single Redirect · Edit** for the redirect;
- **Zone · Zone WAF · Edit** for the port rule.

When Cloudflare refuses a call for lack of either (HTTP 401/403, or codes
10000, 10001, 9109), the step ends `skipped`, nothing is journaled, and its
detail names the missing permission, the DNS provider whose token lacks it,
what stays exposed, and the fix: "Not set up: the API token of DNS provider
`Cloudflare example.com` lacks Zone · Single Redirect · Edit, so
`http://myapp.example.com` still answers without encryption. Add that
permission to the token in Cloudflare (My Profile → API Tokens), then make
the address reachable again to add the rule." The run carries on and
publishes the address. Other refusals (a zone at its rule limit, an invalid
expression) degrade the same way with Cloudflare's own message. A call whose
outcome is unknown (no answer, a server error) is never treated as refused:
the entry stays `pending`, is resolved by lookup, and the rule is never
created twice.

The token field's help text and the provider requirements list the two
permissions as optional ("for HTTPS only"), so a new token can carry them
from the start. The token check (D8) does not probe them yet: a missing one
does not stop a run, and the run says so where it matters.

### D4. The check says when plain HTTP answers, according to the choice

ADR 064 D7's public check gains one probe for a reachable public or signed-in
address whose way in is a tunnel: `GET http://<host>/`, with the same outbound
policy as the gate probe (only addresses the policy accepts, pinned, no
proxy, no redirects followed, 5 s). A redirect, a refusal, or no answer is
never a finding. A `2xx` answer depends on the address's recorded choice; in
every case the verdict stays `reachable` — visitors do reach it:

| Recorded choice | Probed | Plain HTTP answers `2xx` |
|---|---|---|
| `https_only` | yes | **warning** `plain_http_served`: a rule is missing or failed. Appended to the verdict's reason, with a *Make it HTTPS only* action (running it again adds the missing rule). |
| none (an address made reachable before this ADR) | yes | **info** `plain_http_not_chosen`: a gentle note that it answers plain HTTP and that *Make it reachable* can make it HTTPS only. The reason is unchanged; the action is offered. |
| `allow_http`, `allow_all` | no | **info** `plain_http_allowed` without a probe: "it also answers plain HTTP, as you chose". Not a problem, no action, reason unchanged. |

Findings carry a `level` (`info` or `warning`) so a client shows only
warnings as problems.

### D5. Non-Cloudflare paths: what Temps' own proxies do with plain HTTP

Checked while writing this ADR:

- **The control plane's proxy** (`temps-proxy`) redirects plain HTTP to HTTPS
  by the connection itself (`should_redirect_to_https`: no TLS digest, the
  host has a certificate or `force_https`), and deliberately ignores a
  client-supplied `X-Forwarded-Proto` (SEC-12). That is right for a direct
  listener.
- **A worker's public HTTP listener** (`direct` ingress) proxies plain HTTP to
  the app without redirecting.
- **The worker's tunnel origin** is told every request is HTTPS and ignores
  the connector's `X-Forwarded-Proto: http`, so it never redirects either,
  and it tells the app the request was HTTPS when it was not.

Proposed, not built here: the tunnel origin trusts `X-Forwarded-Proto` from
its loopback connector exactly as it already trusts `CF-Connecting-IP` (the
connector, never the visitor, is its peer), forwards the real scheme to the
app, and redirects `http` to `https` for routes whose environment has
`force_https` on, or whose hostname is covered by the edge certificate. The
worker's public HTTP listener gets the control plane's rule (redirect when a
certificate for the host is in the snapshot). Both change answers for every
tunneled and direct hostname, including ones a user deliberately serves over
HTTP and deep subdomains with no edge certificate, so they need their own
per-environment switch and tests; they are not small. Until then D1 covers
tunnel addresses at the edge, and D4 shows any address it does not cover.

## Security

- Nothing zone-wide is changed. Every rule names one hostname, is created by
  one run, carries that run's reference, and is removed by its undo only while
  it still holds what the run wrote.
- Pre-existing rules are never edited, reordered or replaced: rules are only
  added (never `PUT`), an identical rule is used as found, and a rule someone
  edited after the run pauses undo instead of being deleted.
- The hostname is validated before it is placed in an expression.
- The new permissions are zone-scoped, and the token stays limited to one zone
  in this flow (ADR 064 D8). Zone WAF · Edit lets the token change every WAF
  custom rule in that zone, not only Temps'; the help text says so.
- The safer behaviour is the default, and turning it off is an explicit,
  per-address answer whose exposure the plan states; it is never inferred.
- The probe dials only what the outbound policy accepts, pinned against DNS
  rebinding, follows no redirect and reads only the status.

## Consequences

- A new tunnel address made reachable with the default choice and a token
  that has the two permissions is HTTPS-only, on 443 only, from the moment it
  is published. A user who wants plain HTTP turns it off in the plan and gets
  exactly what they chose, with the exposure stated before they apply.
- Existing addresses change in no way. The check adds an informational note
  to one that answers plain HTTP; running *Make it reachable* again offers
  HTTPS only and records the answer.
- A recorded `allow_http`/`allow_all` is respected by every later check and
  offered again by every re-run; only a new answer changes it.
- One nullable column on `project_custom_domains`; no data migration.
- Two optional Cloudflare permissions appear in the token instructions.
- Cloudflare's free-plan rule limits are shared with the user's own rules; a
  zone at its limit degrades with Cloudflare's message.

## Phases

| Phase | Scope |
|---|---|
| 1 | D1–D4: the per-address choice and its column, plan steps, run, journal and undo, degradation without the permissions, the plain-HTTP check |
| 2 | D3 token check probes for the two permissions; D1 rules for environment hosts; D5 tunnel origin and worker HTTP listener redirects behind a per-environment switch |

## Implementation notes (phase 1)

- `reachability/edge_rules.rs` holds `HttpPolicy`, builds the two rules,
  classifies Cloudflare's answers and talks to the Rulesets API through
  `token_check::call`.
- `plan.rs` adds the `http_policy` field and the steps of its default;
  `chosen_http_policy` and `place_http_policy_steps` turn the answer into the
  run's steps and `RunInput.http_policy`; `set_http_policy` records it.
- Migration `m20261102_000001_address_http_policy` adds the column.
- `RunOps` gains `create_edge_rule`, `edge_rule` and `delete_edge_rule`;
  `LiveOps` takes the token of the tunnel's DNS provider (the connector's, or
  the provider managing the hostname's zone) and finds the zone id by walking
  up the hostname.
- The Cloudflare `ref` of a rule is `temps-<install>-run<id>-<https|ports>`;
  the lookup also matches the rule's description, in case Cloudflare stores
  the rule id as its `ref`.
