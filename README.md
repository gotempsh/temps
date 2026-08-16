<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/hero/stop-paying-dark.svg">
  <img alt="Stop paying for 7 SaaS tools: Vercel, Sentry, PostHog, Pingdom, Resend, E2B and Datadog, replaced by one self-hosted Temps binary" src="assets/hero/stop-paying-light.svg" width="700">
</picture>

[Website](https://temps.sh/?utm_source=github&utm_medium=repo&utm_content=header_en) · [Docs](https://temps.sh/docs?utm_source=github&utm_medium=repo&utm_content=header_en) · [Discussions](https://github.com/gotempsh/temps/discussions)

</div>

---

```bash
curl -fsSL https://temps.sh/deploy.sh | bash
```

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/create-dark.png">
  <img alt="Importing a public repository in Temps — framework presets are auto-detected before deploy" src="assets/screenshots/create-light.png">
</picture>

---

## Features

### AI-native — 440+ operations any agent can drive

Every operation in the dashboard is also a CLI command — **440+ of them across 69 groups** — and Temps ships the [skills](skills/) that teach an agent how to use them. Drop them into **Claude Code**, **Codex**, **OpenCode**, or any harness that reads `.claude/skills/`, and your agent can deploy, inspect traces, run migrations, or add a domain without you writing the glue.

```bash
bunx @temps-sdk/cli projects list
bunx @temps-sdk/cli deploy my-app --environment production
bunx @temps-sdk/cli analytics ai-agents -p my-app --period 7d
```

Temps runs those agents for you too: workflow sandboxes execute Claude Code, Codex, or OpenCode against your repository, with platform-wide skills and MCP servers injected automatically.

### AI Chat — grounded in your own telemetry

Ask about your project and the answer comes from your data — traces, metrics, alarms, deployments, and revenue — not from a generic model guess. It is **read-only by default**: write actions are opt-in, and even then the assistant proposes the change and waits for you to confirm.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/ai-chat-dark.png">
  <img alt="Temps AI chat diagnosing a checkout latency spike from the project's own traces, metrics and revenue data" src="assets/screenshots/ai-chat-light.png">
</picture>

### AI Gateway — one endpoint, your own keys

Bring your own provider keys (OpenAI, Anthropic, xAI, Google Gemini) and call them all through one OpenAI-compatible endpoint — swap the base URL, keep the SDK you already use. Keys stay encrypted on your server, and every request is attributed: tokens, latency, error rate, and estimated cost per model.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/ai-gateway-dark.png">
  <img alt="Temps AI Gateway — BYOK provider keys behind one OpenAI-compatible endpoint" src="assets/screenshots/ai-gateway-light.png">
</picture>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/ai-usage-dark.png">
  <img alt="Temps AI Gateway usage analytics — requests, tokens, latency, error rate and estimated cost" src="assets/screenshots/ai-usage-light.png">
</picture>

### Web Analytics & Session Replay

Web analytics with funnels, visitor tracking, and session replay (rrweb) built in — no external services, no data leaving your servers. This is what no other self-hosted PaaS has.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/analytics-dark.png">
  <img alt="Temps web analytics — visitors, sessions, pages, funnels" src="assets/screenshots/analytics-light.png">
</picture>

### Uptime Monitoring & Alerts

Uptime monitors with status timelines, plus alerts for deploy failures, runtime crashes, certificate expiry, and backup health. Get notified before problems reach users.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/uptime-dark.png">
  <img alt="Temps uptime monitoring — status timeline, uptime percentage, response time" src="assets/screenshots/uptime-light.png">
</picture>

### Error Tracking — Sentry-compatible

Drop-in Sentry replacement: point the official Sentry SDK at your Temps DSN and get error groups, stack traces with source context, and alerts. No per-event pricing.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/errors-dark.png">
  <img alt="Temps error tracking — error groups with events and timelines" src="assets/screenshots/errors-light.png">
</picture>

### Request Logs & Proxy Visibility

Every HTTP request logged with method, path, status, response time, and routing metadata — including per-AI-crawler traffic (OpenAI, Anthropic, Perplexity, Google…). Runs on Cloudflare's Pingora engine with auto TLS via Let's Encrypt (HTTP-01 & DNS-01).

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/request-logs-dark.png">
  <img alt="Temps proxy request logs with AI agent filtering" src="assets/screenshots/request-logs-light.png">
</picture>

### Transactional Email

Add sender domains with DKIM records through the UI and send via `@temps-sdk/node-sdk` — or plug in AWS SES, Scaleway, or any SMTP relay.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/email-dark.png">
  <img alt="Temps email providers — SMTP, Scaleway and AWS SES" src="assets/screenshots/email-light.png">
</picture>

### OpenTelemetry — traces, metrics, logs & alerts

Point any OTLP exporter at Temps and get distributed traces, metrics, and structured logs in the same place as everything else. Traces show per-span latency and errors across services; metrics keep your golden signals; alerts fire off those metrics and land in one queue you can acknowledge or resolve. No Grafana, Prometheus, Jaeger, or Loki to run.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/traces-dark.png">
  <img alt="Temps distributed traces — per-request latency, span counts and errors across services" src="assets/screenshots/traces-light.png">
</picture>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/metrics-dark.png">
  <img alt="Temps OpenTelemetry metrics — request rate, latency, database and cache signals" src="assets/screenshots/metrics-light.png">
</picture>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/otel-logs-dark.png">
  <img alt="Temps structured logs — severity, service and message, correlated with traces" src="assets/screenshots/otel-logs-light.png">
</picture>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/alerts-dark.png">
  <img alt="Temps alerts — firing, acknowledged and resolved alarms across metrics, containers, uptime and databases" src="assets/screenshots/alerts-light.png">
</picture>

### AI Sandboxes — Firecracker microVMs, self-hosted

**Real hardware-level isolation, not just containers.** Sandboxes run on **Firecracker microVMs** — the same tech behind AWS Lambda — with a **Docker** backend as the default. Run `temps firecracker setup` and Temps routes sandboxes to microVMs automatically; each one gets its own kernel, so untrusted agent-generated code never shares a kernel with your host.

**A drop-in SDK.** `@temps-sdk/sandbox` is compatible with the `@vercel/sandbox` shape — switch providers by changing the import and base URL:

```ts
import { Sandbox } from '@temps-sdk/sandbox'

const sandbox = await Sandbox.create({
  source: { type: 'git', url: 'https://github.com/example/repo.git', revision: 'main' },
})

const { stdout } = await sandbox.exec(['npm', 'test'])
const url = sandbox.domain(3000) // live preview of a dev server inside the VM
```

**Password-protected previews.** Every sandbox port can be exposed on a public preview URL and locked behind a generated password:

```bash
bunx @temps-sdk/cli sandbox password sbx_abc123 --rotate --length 32
bunx @temps-sdk/cli sandbox password sbx_abc123 --clear   # open it back up
```

Share a running branch without shipping it to the world.

Also available via CLI and REST API. What you'd otherwise pay E2B, Daytona, or Vercel Sandbox for.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/sandboxes-dark.png">
  <img alt="Temps sandboxes — running sandboxes with copy-paste CLI, REST and SDK snippets" src="assets/screenshots/sandboxes-light.png">
</picture>

Each sandbox gets a shell, a preview URL template for any port it binds, and a timeline of everything that happened to it:

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/sandbox-detail-dark.png">
  <img alt="Temps sandbox detail — Docker/Firecracker backend, in-browser command runner, preview URL template and password-protected previews" src="assets/screenshots/sandbox-detail-light.png">
</picture>

### Everything in One Dashboard

Visitors, errors, deployment status, and monitoring health per project — one place instead of six browser tabs.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/dashboard-dark.png">
  <img alt="Temps projects dashboard — all projects with visitors and status" src="assets/screenshots/dashboard-light.png">
</picture>

### Git Push to Deploy & Managed Services

Push to Git and Temps builds, deploys, and creates preview URLs with zero-downtime rollouts — any language, auto-detected. Provision Postgres, Redis, S3 (MinIO), and MongoDB alongside your apps; creation, backups, and teardown are handled for you.

### Works with your stack

<p align="center">
<a href="https://nextjs.org"><img src="https://img.shields.io/badge/Next.js-000?logo=nextdotjs&logoColor=fff&style=for-the-badge" alt="Next.js" /></a>
<a href="https://vitejs.dev"><img src="https://img.shields.io/badge/Vite-646CFF?logo=vite&logoColor=fff&style=for-the-badge" alt="Vite" /></a>
<a href="https://go.dev"><img src="https://img.shields.io/badge/Go-00ADD8?logo=go&logoColor=fff&style=for-the-badge" alt="Go" /></a>
<a href="https://python.org"><img src="https://img.shields.io/badge/Python-3776AB?logo=python&logoColor=fff&style=for-the-badge" alt="Python" /></a>
<a href="https://rust-lang.org"><img src="https://img.shields.io/badge/Rust-000?logo=rust&logoColor=fff&style=for-the-badge" alt="Rust" /></a>
<a href="https://java.com"><img src="https://img.shields.io/badge/Java-ED8B00?logo=openjdk&logoColor=fff&style=for-the-badge" alt="Java" /></a>
<a href="https://dotnet.microsoft.com"><img src="https://img.shields.io/badge/.NET-512BD4?logo=dotnet&logoColor=fff&style=for-the-badge" alt=".NET" /></a>
<a href="https://nestjs.com"><img src="https://img.shields.io/badge/NestJS-E0234E?logo=nestjs&logoColor=fff&style=for-the-badge" alt="NestJS" /></a>
<a href="https://docker.com"><img src="https://img.shields.io/badge/Dockerfile-2496ED?logo=docker&logoColor=fff&style=for-the-badge" alt="Docker" /></a>
</p>

<p align="center"><em>Any language, any framework. Auto-detected or bring your own Dockerfile.</em></p>

---

## Already running somewhere else?

Temps imports your existing setup instead of asking you to rebuild it. Point the wizard at your current platform and it brings over the whole thing — apps, databases *with their data*, domains, and environment variables.

**Self-hosted platforms**

<p align="center">
<a href="https://coolify.io"><img src="https://img.shields.io/badge/Coolify-8B5CF6?logo=coolify&logoColor=fff&style=for-the-badge" alt="Coolify" /></a>
<a href="https://dokploy.com"><img src="https://img.shields.io/badge/Dokploy-0B0B0B?style=for-the-badge" alt="Dokploy" /></a>
<a href="https://caprover.com"><img src="https://img.shields.io/badge/CapRover-2196F3?logo=caprover&logoColor=fff&style=for-the-badge" alt="CapRover" /></a>
<a href="https://portainer.io"><img src="https://img.shields.io/badge/Portainer-13BEF9?logo=portainer&logoColor=fff&style=for-the-badge" alt="Portainer" /></a>
<a href="https://kamal-deploy.org"><img src="https://img.shields.io/badge/Kamal-1B1B1B?style=for-the-badge" alt="Kamal" /></a>
<a href="https://kubernetes.io"><img src="https://img.shields.io/badge/Kubernetes-326CE5?logo=kubernetes&logoColor=fff&style=for-the-badge" alt="Kubernetes" /></a>
<a href="https://docker.com"><img src="https://img.shields.io/badge/Docker-2496ED?logo=docker&logoColor=fff&style=for-the-badge" alt="Docker" /></a>
</p>

**Hosted platforms**

<p align="center">
<a href="https://vercel.com"><img src="https://img.shields.io/badge/Vercel-000?logo=vercel&logoColor=fff&style=for-the-badge" alt="Vercel" /></a>
<a href="https://netlify.com"><img src="https://img.shields.io/badge/Netlify-00C7B7?logo=netlify&logoColor=fff&style=for-the-badge" alt="Netlify" /></a>
<a href="https://railway.app"><img src="https://img.shields.io/badge/Railway-0B0D0E?logo=railway&logoColor=fff&style=for-the-badge" alt="Railway" /></a>
<a href="https://render.com"><img src="https://img.shields.io/badge/Render-000?logo=render&logoColor=fff&style=for-the-badge" alt="Render" /></a>
<a href="https://fly.io"><img src="https://img.shields.io/badge/Fly.io-24175B?logo=flydotio&logoColor=fff&style=for-the-badge" alt="Fly.io" /></a>
</p>

<p align="center"><em>Import runs from the dashboard — the platform tiles sit right in your projects page header.</em></p>

---

## Quick Start

```bash
curl -fsSL https://temps.sh/deploy.sh | bash
```

**Tested on:** Ubuntu 24.04 / 22.04 &nbsp;|&nbsp; Also works on macOS

Prefer not to manage a server? [Temps Cloud](https://temps.sh/pricing?utm_source=github&utm_medium=repo&utm_content=cloud_cta_en) runs Temps for you on managed infrastructure.

---

## What Temps replaces

| What you get | Instead of paying for |
|---|---|
| Git deployments + preview URLs | Vercel / Netlify / Railway ($20+/mo) |
| Web analytics + funnels | PostHog / Plausible ($0-450/mo) |
| Session replay | PostHog / FullStory ($0-2000/mo) |
| Error tracking | Sentry ($26+/mo) |
| Traces, metrics & logs (OpenTelemetry) | Grafana Cloud / Datadog ($0-500+/mo) |
| Uptime monitoring | Better Uptime / Pingdom ($20+/mo) |
| Managed Postgres/Redis/S3 | AWS RDS / ElastiCache ($50+/mo) |
| Transactional email + DKIM | Resend / SendGrid ($20-100/mo) |
| AI code-execution sandboxes | E2B / Daytona / Vercel Sandbox ($150+/mo + usage) |
| AI gateway + usage/cost tracking | OpenRouter / Helicone / LangSmith ($0-200+/mo) |
| Request logs + proxy | Cloudflare ($0-200/mo) |
| **Total with Temps** | **$0 (self-hosted)** |

---

## Temps vs. Alternatives

| Feature | Temps | Coolify | Dokploy | Kamal | Railway | Render | Vercel |
|---|:---:|:---:|:---:|:---:|:---:|:---:|:---:|
| Self-hosted & open source | Yes | Yes | Yes | Yes | No | No | No |
| Single binary install | Yes | No | No | CLI tool | -- | -- | -- |
| Git push deploy | Yes | Yes | Yes | No | Yes | Yes | Yes |
| Preview deployments | Yes | Yes | Yes | No | Yes | Yes | Yes |
| Auto TLS (HTTP-01 + DNS-01) | Yes | Yes | Yes | Yes | Yes | Yes | Yes |
| Docker Compose support | Yes | Yes | Yes | No | -- | -- | -- |
| One-click template library | No | 280+ | Yes | No | Yes | Yes | Yes |
| Web analytics | Yes | No | No | No | No | No | Paid add-on |
| Session replay | Yes | No | No | No | No | No | No |
| Error tracking (Sentry-compatible) | Yes | No | No | No | No | No | No |
| OpenTelemetry traces + metrics + logs | Yes | No | No | No | No | No | Traces (paid) |
| Uptime monitoring | Yes | No | No | No | No | No | No |
| Transactional email + DKIM | Yes | No | No | No | No | No | No |
| Code-execution sandboxes (API) | Yes | No | No | No | No | No | Sandbox (usage-based) |
| AI gateway (BYOK) + assistant | Yes | No | No | No | No | No | AI Gateway (paid) |
| Managed Postgres / Redis | Yes | Yes | Yes | No | Yes | Yes | Partner add-ons |
| S3-compatible storage | Yes | No | No | No | No | No | Blob (paid) |
| Multi-node / clustering | Yes | Yes | Swarm | Yes | Managed | Managed | Managed |
| Edge functions / global edge network | No | No | No | No | No | No | Yes |
| Per-seat fees | No | No | No | No | $20/user (Pro) | Per-user | $20/seat (Pro) |

**Where the alternatives win.** Coolify and Dokploy have one-click template libraries (280+ apps on Coolify) that Temps doesn't have yet, and both have far larger communities — Coolify alone has 56k+ GitHub stars, while Temps is the newest project on this list. Kamal is the simpler choice if all you want is zero-downtime Docker deploys driven from a CLI. Vercel and the other managed platforms give you a global edge network, edge functions, and DDoS absorption that a single VPS can't match — and they run the infrastructure for you, which is real value if you never want to think about a server.

Detailed, regularly updated comparisons: [temps.sh/compare](https://temps.sh/compare?utm_source=github&utm_medium=repo&utm_content=compare_en)

---

## Tech Stack

- **Backend:** Rust, Axum, Sea-ORM, Pingora (Cloudflare's proxy engine), Bollard (Docker API)
- **Frontend:** React 19, TypeScript, Tailwind CSS, shadcn/ui
- **Database:** PostgreSQL + TimescaleDB
- **Architecture:** 30+ workspace crates, three-layer service architecture

---

## SDKs

| Package | Description |
|---|---|
| [`@temps-sdk/node-sdk`](https://www.npmjs.com/package/@temps-sdk/node-sdk) | Platform API client + Sentry-compatible error tracking |
| [`@temps-sdk/react-analytics`](https://www.npmjs.com/package/@temps-sdk/react-analytics) | React analytics, session replay, Web Vitals, engagement tracking |
| [`@temps-sdk/kv`](https://www.npmjs.com/package/@temps-sdk/kv) | Serverless key-value store |
| [`@temps-sdk/blob`](https://www.npmjs.com/package/@temps-sdk/blob) | File storage (S3-compatible) |
| [`@temps-sdk/cli`](https://www.npmjs.com/package/@temps-sdk/cli) | Command-line interface |

<details>
<summary><strong>Quick examples</strong></summary>

**Analytics** -- wrap your React app, everything else is automatic:

```tsx
import { TempsAnalyticsProvider } from '@temps-sdk/react-analytics';

export default function App({ children }) {
  return <TempsAnalyticsProvider>{children}</TempsAnalyticsProvider>;
}
```

**Error tracking** -- Sentry-compatible, drop-in replacement:

```typescript
import { ErrorTracking } from '@temps-sdk/node-sdk';

ErrorTracking.init({ dsn: 'https://key@your-instance.temps.dev/1' });

try {
  riskyOperation();
} catch (error) {
  ErrorTracking.captureException(error);
}
```

**KV store** -- Redis-like API, zero config:

```typescript
import { kv } from '@temps-sdk/kv';

await kv.set('user:123', { name: 'Alice', plan: 'pro' }, { ex: 3600 });
const user = await kv.get('user:123');
```

**Blob storage** -- upload and serve files:

```typescript
import { blob } from '@temps-sdk/blob';

const { url } = await blob.put('avatars/user-123.png', fileBuffer);
const files = await blob.list({ prefix: 'avatars/' });
```

</details>

---

## Community

- [GitHub Discussions](https://github.com/gotempsh/temps/discussions) — questions, ideas, and show & tell
- [GitHub Issues](https://github.com/gotempsh/temps/issues) — bug reports and feature requests

If Temps saves you a SaaS bill, [a star](https://github.com/gotempsh/temps) helps other people find it.

---

## Star History

<a href="https://www.star-history.com/#gotempsh/temps&Date">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/svg?repos=gotempsh/temps&type=Date&theme=dark" />
    <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/svg?repos=gotempsh/temps&type=Date" />
    <img alt="Star History Chart" src="https://api.star-history.com/svg?repos=gotempsh/temps&type=Date" />
  </picture>
</a>

---

## Contributing

We welcome contributions. See [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

```bash
git clone https://github.com/gotempsh/temps.git
cd temps
cargo build --release
```

---

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache 2.0](LICENSE).

[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE)
[![GitHub Release](https://img.shields.io/github/v/release/gotempsh/temps)](https://github.com/gotempsh/temps/releases)
[![Rust](https://img.shields.io/badge/rust-1.70%2B-orange.svg)](https://www.rust-lang.org/)
[![GitHub Stars](https://img.shields.io/github/stars/gotempsh/temps?style=social)](https://github.com/gotempsh/temps)

---

<div align="center">

[temps.sh](https://temps.sh/?utm_source=github&utm_medium=repo&utm_content=footer_en) | [Documentation](https://temps.sh/docs?utm_source=github&utm_medium=repo&utm_content=footer_en) | [GitHub](https://github.com/gotempsh/temps)

English | [简体中文](README.zh-CN.md) | [Español](README.es.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [日本語](README.ja.md) | [Português](README.pt-BR.md)

</div>
