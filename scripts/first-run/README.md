# First-run suite and quiet-logs soak

Two checks that a brand-new Temps install works the way a first-time operator
expects, run nightly by [`.github/workflows/first-run.yml`](../../.github/workflows/first-run.yml)
(04:15 UTC, after the nightly release cut), on manual dispatch, and on pull
requests that change the suite itself.

Both start from nothing: the `temps` binary, an empty database, and the
initial admin created from the one-shot bootstrap variables
(`TEMPS_ADMIN_EMAIL` + `TEMPS_ADMIN_PASSWORD_FILE`). There is no `temps setup`
and no database seeding; the API key the suites use is minted by logging in as
that admin over HTTP (`temps-instance.sh mint-key`).

## First-run suite

`scripts/first-run/run-first-run.sh` runs two parts and writes
`first-run-report.{json,md}` with each step's time to first successful
response through the proxy:

| Step | What a user does | Passes when |
| --- | --- | --- |
| `docker-image` | Docker-image project from a public image | the image answers through the proxy |
| `git-dockerfile` | project from a public git URL, built from `examples/first-run/dockerfile-app/Dockerfile` | the built app returns `{"app":"first-run-dockerfile"}` |
| `node-preset` | uploads `examples/first-run/node-app` (`temps deploy drop`); Temps detects the preset | the app returns `{"app":"first-run-node"}` |
| `compose` | uploads `examples/first-run/compose-app` (two services) | the `web` service answers |
| `static-site` | uploads `examples/first-run/static-site` as a static bundle | the page is served |
| `managed-services` | creates managed Postgres and Redis, links both to the Node app, redeploys | the app sees `POSTGRES_URL` and `REDIS_URL` and can open a TCP connection to each |
| console (Playwright) | creates a **Flexible** project from a Docker image in the UI | the deployment completes and answers through the proxy |

A failed step reports the deployment's classified failure (same classes as
the server's deploy-failure telemetry: `build_error`, `health_check`,
`image_missing`, `timeout`, ...), the job that failed, and its last 40 log
lines; the suite then moves on to the next step.

`git-dockerfile` clones the sample app from GitHub
(`FIRST_RUN_GIT_URL`/`FIRST_RUN_GIT_BRANCH`, default this repository's
`main`). In CI it clones the branch under test. A local git server or
`file://` URL cannot be used: Temps only clones public `https://` remotes on
non-private addresses (`validate_git_url`).

Run a subset with `FIRST_RUN_SCENARIO_ARGS="--only compose static-site"`, skip
the browser part with `FIRST_RUN_SKIP_UI=1`. The scenario itself is
`apps/temps-e2e`'s `first-run-scenario` command, usable against any instance.

## Quiet-logs soak

`scripts/first-run/quiet-logs-soak.sh` builds a small steady-state workload
(one deployed app, one managed Postgres linked to it, one error alert rule,
one uptime monitor), waits a minute for it to settle, leaves the server idle
for `SOAK_MINUTES` (default 20) and then runs `check_quiet_logs.py` on the
server log. Before accepting the report, it verifies that the recorded app still responds successfully and its managed Postgres service is running. A disappeared or unhealthy workload fails the soak even when the log budget passes.

It fails when:

- the log contains **any** `ERROR` line or Rust panic (anywhere in the log,
  including startup), or
- during the idle window any single module logs more than `WARN_PER_HOUR`
  (default 12) `WARN` lines per hour.

The summary lists every ERROR (grouped by shape) and WARN counts per module.
The server runs with `TEMPS_LOG_FORMAT=full`, which puts the module path on
every line.

### Allowlist

Known, tracked exceptions go in
[`quiet-logs-allowlist.toml`](quiet-logs-allowlist.toml). Every entry needs a
`pattern`, a `level`, a `reason` and an `issue` link to the GitHub issue
tracking its removal; the file format is documented at its top. Entries that
match nothing are listed in the summary so they can be deleted once fixed.

### 24-hour soak

Hosted runners stop a job after 6 hours, so the full-day soak runs on a
machine you control:

```bash
SOAK_MINUTES=1440 scripts/first-run/macos-colima.sh quiet-logs   # macOS
```

or on Linux with an instance from `temps-instance.sh` (below) and
`SOAK_MINUTES=1440 scripts/first-run/quiet-logs-soak.sh`. A manual dispatch of
the workflow accepts `soak_minutes` up to about 320.

## Running locally

### Linux

```bash
export TEMPS_BIN=$PWD/target/release/temps          # cargo build --release --bin temps
export DATABASE_URL=postgres://temps:temps@localhost:5432/temps_first_run   # empty DB
scripts/first-run/temps-instance.sh start
export TEMPS_URL=http://127.0.0.1:8760
export TEMPS_API_KEY="$(scripts/first-run/temps-instance.sh mint-key)"
E2E_EMAIL=admin@localho.st E2E_PASSWORD="$(cat /tmp/temps-first-run/admin-password)" \
  scripts/first-run/run-first-run.sh
scripts/first-run/quiet-logs-soak.sh
scripts/first-run/temps-instance.sh stop
```

The scenario packages need the same one-time setup as the other
`apps/temps-e2e` scenarios (see its README): build and `bun link`
`packages/api` and `sdks/node/packages/node-sdk`, then `bun install` in
`apps/temps-e2e` (and in `web/` for the Playwright part).

### macOS with Colima

GitHub's macOS runners have no Docker, so there is no macOS CI job; run the
same suite on a Mac with:

```bash
colima start --cpu 4 --memory 8
scripts/first-run/macos-colima.sh            # both; or `first-run` / `quiet-logs`
```

The script picks up Colima's Docker socket from the active `docker context`,
builds `temps` unless `TEMPS_BIN` is set, starts a throwaway TimescaleDB
container unless `DATABASE_URL` is set, runs the suites, and removes the
server and that container afterwards (`KEEP_RUNNING=1` keeps them). It works
the same with Docker Desktop or OrbStack. Reports are written to the
`FIRST_RUN_DIR` it prints.
