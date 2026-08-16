# Console end-to-end tests

Playwright specs that drive the Temps console in a real browser, against a real
running instance.

## Why this exists

Every other gate we run is static. `cargo build`, `tsc --noEmit` and `rsbuild
build` all pass on a console that renders a blank white page, and the only
runtime check in CI was:

```bash
curl -s -o /dev/null -w "Console HTTP %{http_code}" http://localhost:8081/
```

which returns **200 even when React has died on boot** -- the HTML shell is
served fine, the JS is what fails. That is how issue #504 (react/react-dom on
mismatched versions, React error #527) reached a nightly release and blanked the
console for everyone on `deploy.sh --channel nightly`.

These specs close that gap: they assert the app *mounted*, that the browser
console is clean, and that the core flows a first-run user hits actually work.

## Layout

```
e2e/
  fixtures.ts                 shared fixtures: console-error capture, HTTP
                              failure capture, login helper, URL matchers,
                              uniqueSlug() for parallel-safe resource names
  auth.setup.ts               logs in once, saves storage state for reuse
  anonymous/                  specs that must NOT have a session
    console-boot.spec.ts      the #504 regression guard: app mounts, no JS errors
    auth.spec.ts              login success/failure, route gating
    command-palette.spec.ts   command palette search/navigation
  authenticated/               specs that reuse the saved session
    navigation.spec.ts        every sidebar destination renders real content
    project-create.spec.ts    creates a project from a public git URL
    api-key-create.spec.ts    RBAC role-assignment wizard; the minted secret authenticates
    user-creation.spec.ts     user creation + team-assignment retry (route-mocked)
    drop-handoff.spec.ts      empty-state ZIP/folder upload handoff (route-mocked)
    ai-chat-layout.spec.ts    AI chat page layout
    preview-share-link.spec.ts sandbox preview share-link minting + redemption
```

The anonymous/authenticated split is enforced by `playwright.config.ts` rather
than by convention, because getting it wrong is silent: an "unauthenticated"
assertion that quietly inherits a logged-in storage state still passes while
testing nothing.

## Parallel safety and idempotency

`fullyParallel: true` — specs share one backend, so every spec must be safe to
run concurrently with itself (retries) and with every other spec:

- **Prefer route-mocking** (`page.route(...)`) for anything that doesn't need
  to prove a real server-side effect. Most specs here (`user-creation`,
  `drop-handoff`, `command-palette`) never touch the real backend at all, so
  they have no shared-state exposure by construction.
- **Specs that do create real backend state must generate worker/retry-unique
  names** via `uniqueSlug()` in `fixtures.ts` (not a CI run id alone, which is
  identical across every worker in one run) **and delete what they created in
  a `try/finally`**, so a failed assertion still leaves the instance clean —
  see `project-create.spec.ts` and `preview-share-link.spec.ts` for the
  pattern. A spec that leaks state doesn't just fail once; it corrupts every
  later run that lists or counts the same resource type.
- Genuinely singleton flows (the one seeded admin session `auth.setup.ts`
  creates) live in their own serial Playwright `project` with a `dependencies`
  edge, which Playwright always runs to completion before any parallel spec
  starts — this is the one place serialization is correct rather than a
  workaround.

## Coverage vs. the feature catalog

Tracks `temps/docs/feature-catalog/README.md`'s 6 subsystems. "CLI" refers to
`apps/temps-e2e` scenarios. Most (`scenario`, `examples`, `tls-scenario`,
`email-scenario`) drive `apps/temps-cli`'s underlying API directly via the
SDK client — API-parity coverage, validating the same paths
`bunx @temps-sdk/cli` would exercise, but not the CLI binary itself.
`cli-scenario` is different: it spawns the **real `@temps-sdk/cli` binary as
a subprocess** for every step, so it's genuine CLI coverage — argv parsing,
command wiring, and stdout/`--json` formatting, not just the API underneath
it. This table is the source of truth for gaps — update it whenever a spec
or scenario is added, rather than letting coverage silently drift from the
catalog.

| Subsystem | UI (`web/e2e`) | CLI (`apps/temps-e2e`) |
|---|---|---|
| Deployment & Infrastructure | `project-create`, `navigation`, `drop-handoff`, `domain-tls-pebble` | `scenario`, `examples`, `tls-scenario`, `cli-scenario` |
| Observability | — | — |
| Data & Storage | `navigation` (databases page only) | — |
| AI | `ai-chat-layout` | — |
| Security & Auth | `auth`, `user-creation`, `api-key-create` | `cli-scenario` (apikeys read paths) |
| Platform & Commerce | `command-palette`, `preview-share-link`, `email-provider-mailpit` | `email-scenario` |

Rows with only a dash are not covered end-to-end yet — tracked gaps, not
silently-assumed coverage. Observability, data-storage (beyond the page
rendering), and security-auth (RBAC, audit logs) are the priority next
additions.

`domain-tls-pebble` and `email-provider-mailpit` need the local Pebble +
Mailpit test infra from `apps/temps-e2e/docker-compose.e2e.yml` and a target
instance actually configured to talk to Pebble (see that package's README —
`## External-service test infra`). They're a genuine ACME HTTP-01 exchange
and a genuine SMTP send, not mocked: `domain-tls-pebble` parses the certificate
the console displays and asserts the issuer is Pebble's test root, and
`email-provider-mailpit` queries Mailpit's own REST API to confirm the test
email actually arrived, rather than trusting that the send call returned 200.

**Known gap found while building this coverage**: the console has no UI path
to attach a provisioned certificate (`/domains`) to a project's custom-domain
route (`POST /projects/{id}/custom-domains/{domain_id}/link-certificate/{cert_id}`
is API-only) — so "deploy an app and actually serve it over a custom HTTPS
domain" isn't reachable through the UI today. `domain-tls-pebble` covers what
the UI does offer (standalone certificate provisioning); the full
app-serving path is covered via the API in `tls-scenario`.

## Mocking third-party services

[`vercel-labs/emulate`](https://github.com/vercel-labs/emulate) is available
for test-harness-side use in `apps/temps-e2e` (calling a mocked GitHub/AWS/etc.
API directly from test setup code), but it **cannot** be wired into temps' own
server-side provider config (git provider `base_url`/`api_url`, webhook and
Slack notification targets, custom domains). All of those pass through
`temps_core::url_validation::validate_external_url`, which deliberately
rejects loopback/private/link-local addresses as an SSRF guard — that's a
security boundary, not an oversight, and it is not to be weakened for test
convenience. Practically: real outbound integrations (git clone-by-URL, etc.)
keep hitting real, public, stable third parties in these specs.

## Running locally

The specs need a running Temps instance; they deliberately do not start one.

Bring one up with the `start-temps` skill, then point the suite at that slot's
console port (slot N serves the console on `8081 + N*10`):

```bash
cd web
E2E_BASE_URL=http://localhost:8141 \
E2E_EMAIL=dev@temps.sh \
E2E_PASSWORD='TempsDev!6' \
  bun run e2e
```

Useful variants:

```bash
bun run e2e:ui                    # interactive runner
bun run e2e -- --headed           # watch it drive a real browser
bun run e2e -- e2e/anonymous      # just the unauthenticated specs
bun run e2e:report                # open the HTML report after a run
```

First run only: `bunx playwright install chromium`.

`domain-tls-pebble` and `email-provider-mailpit` need a **dedicated** instance,
not a normal `start-temps` slot: `--address` must be `0.0.0.0:5002` specifically
(Pebble's fixed HTTP-01 validation port), plus `ACME_DIRECTORY_URL`/
`ACME_INSECURE`/`TEMPS_TELEMETRY=0` and the Pebble/Mailpit containers from
`apps/temps-e2e/docker-compose.e2e.yml` — see that package's README for the
full setup. Run them on their own (`bun run e2e -- domain-tls-pebble
email-provider-mailpit`) against that instance rather than as part of a normal
slot run.

| Variable | Default | Notes |
|---|---|---|
| `E2E_BASE_URL` | `http://localhost:8081` | Console URL |
| `E2E_EMAIL` / `E2E_PASSWORD` | `admin@localho.st` / `E2eTestPass123!` | Matches `e2e-tests.yml` |
| `E2E_REPO_URL` | `https://github.com/gotempsh/temps.git` | Public repo used by the project-creation spec |

## Note on the console under test

In CI these run against the console **embedded in the release binary**, not a
dev-server build. That distinction matters: #504 only manifested in the built
bundle. If you rebuild the web assets locally you must rebuild the binary too
(`cargo build --profile fast --bin temps --package temps-cli`) before the server
will serve them -- the assets are compiled in, so restarting alone is not enough.

## Conventions

- **Assert on content, never just "something rendered."** The 404 page mounts
  perfectly happily and gives `#root` children. `expectAppMounted()` is a floor,
  not a sufficient assertion.
- **The `consoleErrors` fixture auto-asserts empty at the end of every test.** A
  spec cannot forget to check. To tolerate an expected error, splice it out
  explicitly so it is visible in review.
- **Never widen the console ignore list to silence a failure.** It is short on
  purpose; every entry is a hole in the net, and anything React-related is
  permanently fatal via `isAlwaysFatal()`. If a 4xx is expected, add a precise
  URL to `EXPECTED_HTTP_FAILURES` instead -- those match on the request URL, so
  they can name the endpoint and the reason.
- **Use RegExp URL matchers, not glob strings.** With `baseURL` configured,
  Playwright resolves a glob relative to it, so `waitForURL('**/projects')`
  never matches `http://host/projects` and fails with an opaque 30s timeout.
  Use the exported `URL_PROJECTS` / `URL_LOGIN` / `urlForProject()`.
