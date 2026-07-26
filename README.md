<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="web/public/logo/temps-logo-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="web/public/logo/temps-logo-light.png">
  <img alt="Temps" src="web/public/logo/temps-logo-dark.png" width="280">
</picture>

**The open-source alternative to Vercel + Sentry + PostHog + Pingdom + Resend + E2B**

Deployments, analytics, session replay, error tracking, uptime monitoring, transactional email & AI sandboxes — one self-hosted binary.

[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE)
[![GitHub Release](https://img.shields.io/github/v/release/gotempsh/temps)](https://github.com/gotempsh/temps/releases)
[![Rust](https://img.shields.io/badge/rust-1.70%2B-orange.svg)](https://www.rust-lang.org/)
[![GitHub Stars](https://img.shields.io/github/stars/gotempsh/temps?style=social)](https://github.com/gotempsh/temps)

[Website](https://temps.sh) · [Documentation](https://temps.sh/docs) · [Quick Start](https://temps.sh/docs/introduction) · [Discussions](https://github.com/gotempsh/temps/discussions) · [Contributing](CONTRIBUTING.md)

English | [简体中文](README.zh-CN.md) | [Español](README.es.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [日本語](README.ja.md) | [Português](README.pt-BR.md)

</div>

---

```bash
curl -fsSL https://temps.sh/deploy.sh | bash
```

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/create-dark.png">
  <img alt="Importing a public repository in Temps — framework presets are auto-detected before deploy" src="assets/screenshots/create-light.png">
</picture>

Stop paying for 7 different SaaS tools. Temps replaces your deployment platform, analytics, error tracking, session replay, uptime monitoring, transactional email, and AI code-execution sandboxes -- all self-hosted, all in one binary.

---

## Features

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
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/alerts-dark.png">
  <img alt="Temps alerts — firing, acknowledged and resolved alarms across metrics, containers, uptime and databases" src="assets/screenshots/alerts-light.png">
</picture>

### AI Sandboxes — isolated code execution

Spin up isolated sandboxes for agent work, tests, and one-off commands via CLI, REST API, or SDK — a Vercel Sandbox-compatible API with Docker or Firecracker microVM backends. What you'd otherwise pay E2B or Daytona for.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/sandboxes-dark.png">
  <img alt="Temps sandboxes — create isolated sandboxes via CLI, REST API or SDK" src="assets/screenshots/sandboxes-light.png">
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

## Quick Start

```bash
curl -fsSL https://temps.sh/deploy.sh | bash
```

**Tested on:** Ubuntu 24.04 / 22.04 &nbsp;|&nbsp; Also works on macOS

Prefer not to manage a server? [Temps Cloud](https://temps.sh/pricing) runs Temps for you on managed infrastructure.

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
| Managed Postgres / Redis | Yes | Yes | Yes | No | Yes | Yes | Partner add-ons |
| S3-compatible storage | Yes | No | No | No | No | No | Blob (paid) |
| Multi-node / clustering | Yes | Yes | Swarm | Yes | Managed | Managed | Managed |
| Edge functions / global edge network | No | No | No | No | No | No | Yes |
| Per-seat fees | No | No | No | No | $20/user (Pro) | Per-user | $20/seat (Pro) |

**Where the alternatives win.** Coolify and Dokploy have one-click template libraries (280+ apps on Coolify) that Temps doesn't have yet, and both have far larger communities — Coolify alone has 56k+ GitHub stars, while Temps is the newest project on this list. Kamal is the simpler choice if all you want is zero-downtime Docker deploys driven from a CLI. Vercel and the other managed platforms give you a global edge network, edge functions, and DDoS absorption that a single VPS can't match — and they run the infrastructure for you, which is real value if you never want to think about a server.

Detailed, regularly updated comparisons: [temps.sh/compare](https://temps.sh/compare)

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

---

<div align="center">

[temps.sh](https://temps.sh) | [Documentation](https://temps.sh/docs) | [GitHub](https://github.com/gotempsh/temps)

</div>
