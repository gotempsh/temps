# ADR 069: *Make it reachable* is a page, and asks what is possible first

## Status

Proposed — 2026-10-28

Related: ADR 064 (making a project reachable: D1 one flow, D3 per-address
reachability, D4 plans, runs, resume and undo), ADR 065 (tailnet names,
signed-in addresses), ADR 068 (environment addresses, D5 the deploy warning).

## Context

ADR 064 D1 put *Make it reachable* in a modal dialog opened from the project
header, the Domains page, the end of a deploy and, since ADR 068 D5, the
deploy warning's tunnel fix. Three things make a modal the wrong container:

1. **A run outlives the dialog.** One run can create a tunnel, DNS records, a
   certificate, a Tailscale Serve entry or Service and a Cloudflare Access
   app, and can wait minutes for a tailnet admin's approval. ADR 064 D4 keeps
   the run on the server, but closing the dialog loses the progress view, and
   the only way back is to reopen the dialog and hope it adopts the run.
2. **It cannot be linked.** Every entry point mounts its own copy of the
   dialog with its own props. The uptime monitor's "Nothing to check" state
   has no way to offer it at all, and nobody can paste a link to "make
   staging private" into a chat.
3. **It dead-ends.** The user picks an audience, types an address, and only
   then is told, for example, that production runs on the control plane and a
   tunnel only carries what workers serve. The planner knew this before the
   user typed anything: `GET …/reachability` already plans the public
   audience with no address. Nothing asks it about the other audiences.

## Decision

### D1. One route per environment

```
/projects/:slug/environments/:environment/reachability
```

`:slug` is the project slug, as every project route. `:environment` is the
environment id, which every link Temps generates uses; an environment name
is accepted too, so a hand-typed `…/environments/staging/reachability`
works. It is a sibling of the existing `/projects/:slug/environments` view
(which selects an environment with `?environment=`), registered before its
catch-all.

### D2. What goes in the URL

| Parameter | Values | History |
|---|---|---|
| `audience` | `public`, `signed_in`, `private`, `tailnet_users` | push |
| `gate` | `temps`, `cloudflare_access` (only with `signed_in`) | push |
| `address` | the hostname to plan for | replace, as typed |
| `way` | a plan option kind to preselect when offered | replace |
| `run` | a run id: show that run instead of the form | push |

Choosing an audience or a gate pushes a history entry, so Back walks the
user's choices; typing an address replaces the entry rather than adding one
per keystroke. Starting a run pushes `?run=<id>`, so Back returns to the
plan and the URL is the link to the run. Unknown values are ignored, not
errors: an `audience` this version does not offer falls back to the first
available one.

Nothing secret or personal goes in the URL. Tokens, OAuth clients, the
tailnet users and tags a `tailnet_users` address admits, and Access emails
stay in the form.

### D3. The page asks what is possible before the user chooses

`GET /projects/{project_id}/environments/{environment_id}/reachability/choices`
returns, per audience (and per gate for `signed_in`):

- `available`, and when not, `blocked`: the planner's reason and fix (the
  same `PlanBlocked` a plan carries);
- `ways_in`: the options the planner offers with no address asked for
  (kind, title, way in, recommended), and the address it would suggest;
- `not_offered`: ways in it left out and why, when the audience is still
  available another way: a tunnel for an environment on the control plane,
  or for one governed by an IP allow list of only private ranges that no
  visitor through a tunnel can match (ADR 064 D9).

It runs the same planner as `POST …/plan` over the same gathered facts, with
no address, and stores nothing: no `plan_id`, no fingerprint. A
`tailnet_users` audience is judged on whether any way in can tell who a
visitor is, before anyone is listed. The answer is advisory. The page still
plans the address the user types, and `apply` still holds the run to that
plan's fingerprint (ADR 064 D4), so a cluster that changed between the two
is caught exactly where it was before.

Audiences that cannot work are shown disabled, with that reason and its fix,
never hidden (Feature Discoverability).

### D4. Sections

1. **Who can open it.** Audience cards from D3.
2. **Address and how visitors get in.** The address (prefilled with the
   suggestion) and the ways in the plan offers for it: worker, tunnel,
   Tailscale, with how DNS is handled. Ways in the planner left out are
   listed with why.
3. **Plan.** The chosen option's steps, rendered generically from the API
   (label per step; the step kind only picks an icon, with a default), so a
   new step kind needs no console change. Beside it, what Temps leaves
   alone: the environment's other addresses, DNS records the plan does not
   list, and anything found already there, which undo never removes
   (ADR 064 D4).
4. **Run.** Live progress, resume, conflicts to decide, and undo.

### D5. Runs are found again, not adopted

A run is server-side (ADR 064 D4) and its URL is `?run=<id>`. The page polls
it while it is active and every 15 seconds while it waits. Opening the page
while the environment's newest run is unfinished (running, waiting,
interrupted, in conflict, or with undo paused) shows a notice linking to it
rather than silently switching to it; starting over is just the URL without
`run`. The Domains page's *Who can reach it* list links each environment's
newest run, so a run is reachable later from where its result shows up.

### D6. Entry points are links

The project header, *Who can reach it*, the end of a deploy, the deploy
warning's tunnel fix (`?address=<host>&way=tunnel_environment_hosts`) and the
uptime monitor's "Nothing to check" state link to the route. The modal is
deleted.

## Security

- `GET …/choices` needs the same `projects:read` permission and project scope
  as `GET …/reachability`, and reveals nothing that endpoint and `POST …/plan`
  do not already return to the same caller.
- It makes the same provider reads a plan makes (DNS records at the
  suggested address, Access apps covering it, the Tailscale Service name),
  once per audience. No writes, no rows.
- No credential or principal is put in the URL (D2).

## Consequences

- A run can be watched, shared and returned to; closing a tab loses nothing.
- Opening the page costs up to five unstored plans. It is a page the user
  opens on purpose, not a header polled every minute.
- The capability answer can be stale by the time the user applies; apply's
  fingerprint check (ADR 064 D4) is still the guard.
