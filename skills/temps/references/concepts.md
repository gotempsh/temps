# Temps concepts and terminology

The vocabulary below is used consistently across every Temps skill and across the dashboard, CLI, and API. Get it right once instead of re-deriving it per task.

## The hierarchy

```
Project
 └─ Environment (e.g. production, staging, preview)
     └─ Deployment (one build+release of the app)
         └─ Container(s) (the running instance(s))
```

- **Project** — one application. Backed by a Git repository (auto-deploy on push, framework auto-detected) or a manual source (a Docker image or a static-files archive pushed directly). A project has one or more environments.
- **Environment** — an isolated instance of the project with its own environment variables, domain(s), and deployment history. `production` always exists; `staging` and per-branch preview environments are common additions.
- **Deployment** — a single build-and-release: one commit/image, built, health-checked, and rolled out to an environment. Deployments can be rolled back to.
- **Service** — a managed dependency attached to a project: PostgreSQL, MySQL, MongoDB, Redis, or S3-compatible object storage. Provisioned and connected via the CLI/dashboard, not hand-rolled.
- **Container** — the actual running process(es) for a deployment, orchestrated via Docker. Replicas, health checks, and graceful shutdown all operate at this level — see [temps-best-practices](../../temps-best-practices/SKILL.md) for the runtime contract containers must satisfy.

## Deployment sources

| Source | How it works | Config |
|---|---|---|
| Git repository | Push to a connected branch (or run `temps deploy` / `temps up`); Temps clones, detects the framework, builds, and rolls out | `.temps.yaml` at the effective root directory |
| Docker image | Point a manual project at a pre-built image; no repository needed | Deployment `health_check_path` / CLI `--health-check-path` override (can't read `.temps.yaml` — there's no repo to read it from) |
| Static files | Upload a tar.gz/zip/directory of pre-built static assets | Same as image deploys — no `.temps.yaml` |

## `.temps.yaml`

Lives in the project's root directory (or the effective Temps Root Directory / Docker build context, for monorepos). The field that actually takes effect today is the health check path:

```yaml
health:
  path: /healthz
```

Other fields may parse without error but aren't yet applied — see [temps-best-practices/references/runtime-contract.md](../../temps-best-practices/references/runtime-contract.md) for the current, authoritative list of what's live versus parsed-only, plus everything else the runtime contract requires (`PORT`/`HOST` binding, `SIGTERM` handling, readiness semantics, migrations).

## Networking and TLS

Temps' data plane is a Pingora-based reverse proxy: it terminates TLS (automatic certificates via Let's Encrypt), routes by host to the right project/environment/container, and handles custom domains including apex and wildcard domains — see [add-custom-domain](../../add-custom-domain/SKILL.md).

## Observability surfaces

Temps replaces several single-purpose SaaS tools with one ingestion surface, each mapping to a "pillar":

| Pillar | Replaces | Skill |
|---|---|---|
| Error tracking | Sentry (Sentry-compatible DSN/SDK, no code changes beyond init) | [add-error-tracking](../../add-error-tracking/SKILL.md) |
| Traces / metrics / logs | Datadog / Honeycomb (OTLP ingestion) | [temps-best-practices](../../temps-best-practices/SKILL.md) |
| Analytics + session replay | PostHog / Plausible / FullStory | [add-react-analytics](../../add-react-analytics/SKILL.md), [add-session-recording](../../add-session-recording/SKILL.md) |
| Uptime monitoring | Pingdom | `temps-cli` → Monitoring command group |

Auth for all of these is scoped by token type — a `dt_` deployment token, a `tk_` API key, or (for error tracking specifically) a Sentry-style DSN public key. None of them belong in a client bundle except the DSN and the public-prefixed analytics/Sentry variables — see the credential-boundary rules in [temps-best-practices](../../temps-best-practices/SKILL.md).

## Auth tokens, at a glance

| Prefix | What it is | Where it's used | Client-safe? |
|---|---|---|---|
| `dt_` | Deployment token — server/environment-scoped | OTLP ingestion, deploy hooks | No |
| `tk_` | API key | CLI auth, OTLP ingestion (needs `X-Temps-Project-Id`) | No |
| `si_` | Integration token | Infrastructure metrics ingestion | No |
| DSN public key | Sentry-compatible error tracking | Client and server error SDKs | Yes — designed for it |

## Self-hosted vs. Temps Cloud

Same binary, same feature set. The only difference is who operates the control plane:

- **Self-hosted** — you run the `temps` binary (or the install script) on your own infrastructure. Free.
- **Temps Cloud** — Temps runs it for you; provisioned and managed from the dashboard. Paid, managed hosting.

Nothing in application code, `.temps.yaml`, or the CLI needs to change between the two — don't write Cloud-specific or self-host-specific branches unless a skill says otherwise.

## Multi-node

A Temps control plane can join worker nodes over WireGuard to run containers across multiple machines. This is an advanced/optional topology — most single-server self-hosted installs never touch it. See `temps-cli`'s platform administration commands if it comes up.
