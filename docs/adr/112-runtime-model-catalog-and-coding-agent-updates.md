# ADR 112: New models and coding-agent releases without a Temps release

## Status

Accepted — 2026-11-23.

Related: ADR 037 (AI CLI providers), ADR 054 (autofix incidents in
workspaces).

## Context

Every AI model Temps knew about was compiled into the binary: the price table
(`crates/temps-ai-gateway/src/services/pricing/mod.rs`), the bootstrap model
lists, capability rules and the pickers. Providers publish models every few
weeks. An instance that did not upgrade kept offering, and pricing, the models
of its release date:

- a newer model could be used through the gateway, but was charged at the
  worst-case rate with no way to say why;
- the coding-agent CLIs in a user's persistent workspace (Claude Code, Codex,
  OpenCode) stayed at the version the workspace image bundled, and an old CLI
  cannot offer models released after it;
- the workspace image pinned those CLI versions, so even a fresh workspace
  started out of date.

Operators of self-hosted instances upgrade on their own schedule, often months
apart. Model availability and prices must not depend on that schedule.

## Decision

### D1. A runtime model catalog layered over the bundled table

- **Sources, in order of precedence**: the catalog stored in the database, then
  the bundled table. The bundled table stays the fallback for everything the
  catalog does not list, so an instance that never reaches a catalog behaves
  exactly as before.
- **Remote catalog**: a JSON document in the models.dev format, by default the
  one published in this repository
  (`crates/temps-ai-gateway/catalog/model-catalog.json`, generated from the
  bundled table and kept identical to it by a test). Billing data therefore
  still lives in this repository and is reviewed like code; the URL can point
  at models.dev or a mirror instead.
- **Validation**: a typed schema; only the providers the gateway serves are
  read and everything else is skipped without being parsed into memory. A row
  is rejected when a price is negative, not finite, or above
  $10,000 per 1M tokens. A catalog can lower a price but never make a paid
  model free, and the bundled long-context tiers stay in force, including for
  a dated snapshot of a bundled model. A model id listed under two providers
  keeps each provider's price: usage is recorded at the serving provider's
  price, and where only the model is known the dearer one applies, whatever
  the catalog order.
- **Fetch limits**: `https://` only, no credentials, query string or fragment
  in the URL (it is shown to every gateway reader, stored on each entry and
  logged, so a signed or token-bearing URL is refused), no private,
  loopback or metadata addresses (checked when the URL is saved and again by
  the resolver when connecting), no redirects, 30 s timeout, 16 MiB body,
  20,000 models.
- **Storage**: `ai_model_catalog_entries` (one row per provider and model,
  with `fetched_at` and `source_url`) replaced in one transaction per refresh;
  `ai_model_catalog_settings` (one row) holds the source URL, the refresh
  toggle and interval (1 to 168 hours, default 24) and the outcome of the last
  attempt. Configuration is these columns, not environment variables.
- **Live discovery**: when enabled, the provider keys' list-models endpoints
  are read on the same schedule, so a model the provider serves appears in the
  pickers even before a catalog lists it.
- **Unknown models stay usable**. A request for a model no price covers is
  still served and charged the worst-case rate (so it cannot be used to get
  around a spend limit), and is recorded with `price_known = false`. The
  console lists enabled models without a price.
- **API**: `GET /ai/model-catalog` (status), `PATCH /ai/model-catalog/settings`,
  `POST /ai/model-catalog/refresh` ("Refresh now"), `GET
  /ai/model-catalog/models` (paginated, default 20, max 100) and `GET
  /ai/model-catalog/unpriced`. Writes need `ai_gateway:write` and
  `system:admin`, because a catalog reprices every project's usage, and are
  audit-logged with the source host only.

### D2. Coding-agent CLIs kept current at runtime

- **Latest release**: read from the npm registry
  (`https://registry.npmjs.org/<package>/latest`) for the three packages. The
  host and package names are constants; only a plain `major.minor.patch`
  version of the expected package is accepted. 10 s timeout, 1 MiB body, no
  redirects, cached six hours (a failure is retried after 15 minutes and keeps
  showing the last version known).
- **Installed version and "Update"**: the existing per-workspace
  harness-update endpoints read the version inside the caller's persistent
  workspace and install the latest release with the installer that owns the
  running binary (npm for the runtime image, the vendor's updater for a copy
  under the home directory). An update never runs while a turn holds the
  workspace.
- **Automatic updates**: `ai_agent_cli_settings.auto_update`, off by default,
  changed through `PATCH /ai/coding-agents/settings` (needs `settings:write`
  and `system:admin`, audit-logged). When on, a background pass every 30
  minutes updates the workspaces that ran a turn in the last two hours and are
  behind, at most 10 per pass, each at most once per six hours, skipping a busy
  workspace until the next pass. Each update is audit-logged with the system as
  the actor and the workspace owner, old and new versions.
- **Images**: pull-request and local builds keep the Dockerfile pins, so they
  are reproducible. Published runtime images bundle the latest stable release
  of each CLI at build time, never older than the pin, and record the versions
  in `sh.temps.cli.*` image labels.
- **Model entry**: a gateway key's models come from live discovery, so a
  model newer than the catalog is selectable once the key's models are
  refreshed. The key's enabled-model list stays the operator's policy: an id
  that is not on it is refused, never typed in around it.

### D3. Console

`/ai-gateway/models` shows the catalog source, when it was last refreshed and
the last error, a "Refresh now" button, the settings, the models without a
price, the stored catalog, and the coding agents with their installed and
latest versions, an Update button and the automatic-update toggle. It is
reachable from the AI gateway navigation and the command palette, and renders
an onboarding state when nothing is configured yet.

## Load, memory and failure

All of this is control-plane work; nothing runs on the proxy or gateway
request path.

- **Expected load**: one catalog fetch per refresh interval (default daily),
  plus a retry at most hourly after a failure; one list-models call per
  provider key per interval, sequential, each bounded to two minutes; three
  npm registry requests per six hours; one automatic-update pass every 30
  minutes, touching at most 10 workspaces.
- **Request path cost**: a price lookup reads the effective table through an
  `ArcSwap` load (no lock, no I/O, no allocation). The table is rebuilt and
  swapped only when a refresh stores a new catalog.
- **Memory bound**: the effective table holds one entry per priced model
  (a few hundred in practice, capped by the 20,000-model limit; on the order
  of 100 bytes each). A fetch holds at most the 16 MiB body while parsing.
  The coding-agent state is three cached releases and at most one timestamp
  per (CLI, workspace) used within six hours.
- **Failure behaviour**:
  - catalog unreachable, too large, invalid or empty: the previous catalog and
    the bundled table stay in use, the error is recorded in
    `ai_model_catalog_settings.last_error` and shown in the console, and the
    next attempt is at most an hour later;
  - an invalid row is skipped and counted, never applied;
  - a refresh already running: the manual request gets 409, the background
    one is skipped;
  - npm registry unreachable: the last known version keeps being shown with
    the error; automatic updates still compare against the workspace's own
    check;
  - a workspace update fails or the workspace is busy: nothing else is
    affected; a failure is retried after six hours, a busy workspace on the
    next pass;
  - a failed audit write is logged and the change stands.

## Alternatives considered

- **Fetch models.dev directly by default.** Its schema is the one used here,
  but it is a third-party document; prices drive spend limits, so the default
  is a catalog reviewed in this repository. Operators can still point at
  models.dev.
- **An environment variable for the catalog URL or the auto-update switch.**
  Rejected: configuration is a column, changeable at runtime, audit-logged.
- **Resolving CLI versions at container start.** It would make every
  workspace start depend on the npm registry and run unreviewed code paths at
  boot; updating in place on request, or automatically with a cooldown, keeps
  start-up offline-capable.
- **Updating every workspace automatically.** Waking idle workspaces to update
  them costs memory and CPU on small hosts; only workspaces in use are
  updated, and the rest are updated on their next use.

## Consequences

- New models get prices and appear in pickers without a Temps release.
- Prices can change between releases; every change is attributable to a
  catalog refresh with its source host in the audit log.
- Published runtime images are no longer byte-for-byte reproducible from a
  commit alone; the labels record what they contain.
