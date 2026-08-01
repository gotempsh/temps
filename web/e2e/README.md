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
  fixtures.ts              shared fixtures: console-error capture, HTTP failure
                           capture, login helper, URL matchers
  auth.setup.ts            logs in once, saves storage state for reuse
  anonymous/               specs that must NOT have a session
    console-boot.spec.ts   the #504 regression guard: app mounts, no JS errors
    auth.spec.ts           login success/failure, route gating
  authenticated/           specs that reuse the saved session
    navigation.spec.ts     every sidebar destination renders real content
    project-create.spec.ts creates a project from a public git URL
```

The anonymous/authenticated split is enforced by `playwright.config.ts` rather
than by convention, because getting it wrong is silent: an "unauthenticated"
assertion that quietly inherits a logged-in storage state still passes while
testing nothing.

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
