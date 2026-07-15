<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="web/public/logo/temps-logo-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="web/public/logo/temps-logo-light.png">
  <img alt="Temps" src="web/public/logo/temps-logo-dark.png" width="280">
</picture>

### The open-source, self-hosted deployment platform.
### Deploy, observe, and scale -- from a single binary.

[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE)
[![GitHub Release](https://img.shields.io/github/v/release/gotempsh/temps)](https://github.com/gotempsh/temps/releases)
[![Rust](https://img.shields.io/badge/rust-1.70%2B-orange.svg)](https://www.rust-lang.org/)
[![GitHub Stars](https://img.shields.io/github/stars/gotempsh/temps?style=social)](https://github.com/gotempsh/temps)

[Website](https://temps.sh) | [Documentation](https://temps.sh/docs) | [Quick Start](https://temps.sh/docs/introduction) | [GitHub](https://github.com/gotempsh/temps)

</div>

---

<p align="center">
  <img src="temps-demo.gif" alt="Temps — from bare server to deployed in under 3 minutes" width="800" />
  <br />
  <em>From bare server to fully deployed — in under 3 minutes (166s).</em>
</p>

```bash
curl -fsSL https://temps.sh/deploy.sh | bash
```

![Temps Dashboard](assets/screenshots/dashboard.png)

Stop paying for 6 different SaaS tools. Temps replaces your deployment platform, analytics, error tracking, session replay, uptime monitoring, and transactional email -- all self-hosted, all in one binary.

---

## Features

<table>
<tr>
<td width="50%">

**Git Push to Deploy**
Push to Git, Temps builds and deploys. Auto-detects frameworks, creates preview URLs, and handles zero-downtime rollouts.

![Deployments](assets/screenshots/deployments.png)

</td>
<td width="50%">

**Built-in Analytics & Session Replay**
Web analytics with funnels, visitor tracking, and session replay (rrweb). Sentry-compatible error tracking. No external services.

![Analytics](assets/screenshots/analytics.png)

</td>
</tr>
<tr>
<td width="50%">

**Pingora-Powered Proxy**
Runs on Cloudflare's Pingora engine. Auto TLS via Let's Encrypt (HTTP-01 & DNS-01), custom domains, and full request logging.

![Domains](assets/screenshots/domains.png)

</td>
<td width="50%">

**Managed Services**
Provision Postgres, Redis, S3 (MinIO), and MongoDB alongside your apps. Temps handles creation, backups, and teardown.

![Monitoring](assets/screenshots/monitoring-detail.png)

</td>
</tr>
<tr>
<td width="50%">

**Request Logs & Proxy Visibility**
Every HTTP request logged with method, path, status, response time, and routing metadata. Filter and search without extra tooling.

![Proxy Logs](assets/screenshots/proxy-logs.png)

</td>
<td width="50%">

**Monitoring & Alerts**
Monitors for deploy failures, runtime crashes, certificate expiry, and backup health. Get notified before problems reach users.

![Project Overview](assets/screenshots/project-overview.png)

</td>
</tr>
<tr>
<td width="100%" colspan="2">

**Transactional Email**
Add sender domains with DKIM records through the UI. Send transactional emails via `@temps-sdk/node-sdk`. No external email service needed.

</td>
</tr>
</table>

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

---

## What Temps replaces

| What you get | Instead of paying for |
|---|---|
| Git deployments + preview URLs | Vercel / Netlify / Railway ($20+/mo) |
| Web analytics + funnels | PostHog / Plausible ($0-450/mo) |
| Session replay | PostHog / FullStory ($0-2000/mo) |
| Error tracking | Sentry ($26+/mo) |
| Uptime monitoring | Better Uptime / Pingdom ($20+/mo) |
| Managed Postgres/Redis/S3 | AWS RDS / ElastiCache ($50+/mo) |
| Transactional email + DKIM | Resend / SendGrid ($20-100/mo) |
| Request logs + proxy | Cloudflare ($0-200/mo) |
| **Total with Temps** | **$0 (self-hosted)** |

---

## Temps vs. Alternatives

| Feature | Temps | Coolify | Dokploy | CapRover | Dokku | Railway | Vercel |
|---|:---:|:---:|:---:|:---:|:---:|:---:|:---:|
| Self-hosted | Yes | Yes | Yes | Yes | Yes | No | No |
| Single binary install | Yes | No | No | No | No | -- | -- |
| Git push deploy | Yes | Yes | Yes | Yes | Yes | Yes | Yes |
| Multi-node / clustering | No | Yes | Yes | Yes | No | Yes | Yes |
| Web analytics | Yes | No | No | No | No | Yes | Yes |
| Session replay | Yes | No | No | No | No | No | No |
| Error tracking (Sentry-compatible) | Yes | No | No | No | No | No | No |
| Uptime monitoring | Yes | No | No | No | No | No | No |
| Transactional email + DKIM | Yes | No | No | No | No | No | No |
| Managed Postgres/Redis/S3 | Yes | Yes | Yes | Partial | Plugin | Yes | Add-on |
| Pingora proxy (Cloudflare-grade) | Yes | No | No | No | No | No | No |
| Auto TLS (HTTP-01 + DNS-01) | Yes | Yes | Yes | Yes | Plugin | Yes | Yes |
| Request-level logging | Yes | No | No | No | No | Partial | Partial |
| Preview deployments | Yes | Yes | Yes | No | No | Yes | Yes |
| Built with Rust | Yes | No | No | No | No | No | No |
| Free & open source | Yes | Yes | Yes | Yes | Yes | No | No |

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
