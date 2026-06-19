# @temps-sdk/e2e

End-to-end + load testing CLI for a **live** Temps instance. Drives the real
control-plane API (via the shared [`@temps-sdk/api`](../../packages/api) client),
deploys an app, generates traffic, verifies the proxy recorded it, and tears
everything down.

## Setup

```bash
cd apps/temps-e2e
bun install
```

The tool depends on `@temps-sdk/api` via a local link. If the link isn't set up:

```bash
cd ../../packages/api && bun install && bun run build && bun link
cd ../../apps/temps-e2e && bun install
```

## Auth

Point it at any instance with `--url` / `--api-key`, or the `TEMPS_URL` /
`TEMPS_API_KEY` env vars (default URL: `http://localhost:8080`).

Mint a key against a **local** instance directly from the server binary:

```bash
temps api-key \
  --database-url=postgres://postgres:password@localhost:5432/temps_development \
  --name=e2e --role=admin --user-email=you@example.com --output-format=json
```

## Commands

```bash
# Verify connectivity + auth
bun run src/index.ts ping

# Generate load against any URL (no Temps deploy required)
bun run src/index.ts load https://example.com -n 10000 -c 100
bun run src/index.ts load https://example.com -d 60s -c 200      # by duration
#   -H "Host: app.localho.st"   to route through a proxy by host header

# Full lifecycle: project -> deploy image -> wait healthy -> load -> verify -> teardown
bun run src/index.ts scenario --image traefik/whoami:latest -n 2000 -c 50
bun run src/index.ts scenario --with-db                          # also provision postgres
bun run src/index.ts scenario --keep                            # leave resources up
bun run src/index.ts scenario --json                            # machine-readable (CI)

# Build the repo's example projects (Go, Python, Node, …) and run the full
# deploy/verify lifecycle for EACH — proves every example in examples/ actually
# deploys to a live Temps and serves real traffic (not just a prebuilt image).
#
# Requires Docker + a registry the Temps server can pull from. Start a local one:
#   docker run -d -p 5111:5000 --name temps-e2e-registry registry:2
#   export TEMPS_E2E_REGISTRY=localhost:5111
bun run src/index.ts examples --list                            # show registered examples
bun run src/index.ts examples                                   # fast subset: go-gin, python-flask, node-nestjs
bun run src/index.ts examples --only go-gin python-flask        # pick specific ones
bun run src/index.ts examples --all                             # every example (incl. heavy rust/vite builds)
bun run src/index.ts examples --registry localhost:5111         # registry (or $TEMPS_E2E_REGISTRY)
bun run src/index.ts examples --json                            # machine-readable (CI)
```

### `examples`

Verifies that the source projects under the repo's `examples/` tree actually
deploy and serve on a live Temps. Each registered example (`src/lib/examples.ts`)
carries its source path, a minimal generated Dockerfile, its listen port and a
health path. For every selected example the command:

1. renders the Dockerfile into a scratch build context, `docker build`s it, and
   **pushes it to `$TEMPS_E2E_REGISTRY`** (Temps deploys by pulling, so the image
   must live somewhere the server can reach — same path a real user follows),
2. runs the full `scenario` lifecycle against the pushed image (create project →
   deploy → wait healthy → **assert the real app responds, not the Temps console
   fallback** → load → verify proxy logs → teardown),
3. prints a per-example PASS/FAIL summary and exits non-zero if any fails.

The build context is a scratch copy — the repo's `examples/` tree is never
mutated. Only HTTP-serving examples are registered; `node/vercel-ai-tracing` is
excluded (it's a one-shot OTel script needing LLM keys, not a server).

Verified passing (`--all`, 5/5): Go (Gin), Python (Flask), Node (NestJS),
Vite (React via nginx-unprivileged), Rust (Axum).

### `scenario` steps

1. create a project (`docker_image` source)
2. *(optional `--with-db`)* provision a Postgres external service
3. resolve the production environment
4. deploy a prebuilt public image
5. wait for the deployment to reach a terminal state
6. probe HTTP until the app actually serves (routes via the proxy origin with
   the app's `Host` header — no external DNS/TLS dependency)
7. warm up, then run the measured load test
8. verify the proxy-log count for the host is non-zero
9. tear down the deployment (stops the container + removes the route) and delete
   the project — even on failure, unless `--keep`

Exits non-zero if any step fails, so it's CI-gateable.

## Notes

- The load engine is pure `fetch`, worker-pooled (exactly `--concurrency`
  in-flight). Transient connection failures are retried (`--connectRetries`
  equivalent); real HTTP 4xx/5xx are recorded as-is.
- Resources are name-prefixed `e2e-<runid>` so leftovers are identifiable.
