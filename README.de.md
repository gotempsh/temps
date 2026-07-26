<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="web/public/logo/temps-logo-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="web/public/logo/temps-logo-light.png">
  <img alt="Temps" src="web/public/logo/temps-logo-dark.png" width="280">
</picture>

### Die quelloffene, selbst gehostete Deployment-Plattform.
### Deployen, beobachten und skalieren -- aus einem einzigen Binary.

[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE)
[![GitHub Release](https://img.shields.io/github/v/release/gotempsh/temps)](https://github.com/gotempsh/temps/releases)
[![Rust](https://img.shields.io/badge/rust-1.70%2B-orange.svg)](https://www.rust-lang.org/)
[![GitHub Stars](https://img.shields.io/github/stars/gotempsh/temps?style=social)](https://github.com/gotempsh/temps)

[Website](https://temps.sh) | [Dokumentation](https://temps.sh/docs) | [Schnellstart](https://temps.sh/docs/introduction) | [Diskussionen](https://github.com/gotempsh/temps/discussions)

[English](README.md) | [简体中文](README.zh-CN.md) | [Español](README.es.md) | [Français](README.fr.md) | Deutsch | [日本語](README.ja.md) | [Português](README.pt-BR.md)

</div>

---

<p align="center">
  <img src="temps-demo.gif" alt="Temps — vom nackten Server zum fertigen Deployment in unter 3 Minuten" width="800" />
  <br />
  <em>Vom nackten Server zum vollständigen Deployment — in unter 3 Minuten (166s).</em>
</p>

```bash
curl -fsSL https://temps.sh/deploy.sh | bash
```

![Temps Dashboard](assets/screenshots/dashboard.png)

Schluss mit dem Bezahlen für 6 verschiedene SaaS-Tools. Temps ersetzt deine Deployment-Plattform, Analytics, Error-Tracking, Session Replay, Uptime-Monitoring und Transaktions-E-Mails -- alles selbst gehostet, alles in einem Binary.

---

## Features

<table>
<tr>
<td width="50%">

**Integrierte Analytics & Session Replay**
Web-Analytics mit Funnels, Besucher-Tracking und Session Replay (rrweb). Sentry-kompatibles Error-Tracking. Keine externen Dienste — das bietet keine andere selbst gehostete PaaS.

![Analytics](assets/screenshots/analytics.png)

</td>
<td width="50%">

**Uptime-Monitoring & Alerts**
Uptime-Monitore mit Status-Zeitleisten, dazu Alerts bei fehlgeschlagenen Deployments, Laufzeit-Abstürzen, ablaufenden Zertifikaten und Backup-Problemen. Erfahre von Problemen, bevor deine Nutzer sie bemerken.

![Uptime-Monitoring](assets/screenshots/monitoring-detail.png)

</td>
</tr>
<tr>
<td width="50%">

**Git Push to Deploy**
Push nach Git, Temps baut und deployt. Erkennt Frameworks automatisch, erstellt Preview-URLs und übernimmt Zero-Downtime-Rollouts.

![Deployments](assets/screenshots/deployments.png)

</td>
<td width="50%">

**Alles in einem Dashboard**
Besucher, Fehler, Deployment-Status und Monitoring-Zustand pro Projekt — an einem Ort statt in sechs Browser-Tabs.

![Projektübersicht](assets/screenshots/project-overview.png)

</td>
</tr>
<tr>
<td width="50%">

**Proxy auf Pingora-Basis**
Läuft auf Cloudflares Pingora-Engine. Automatisches TLS via Let's Encrypt (HTTP-01 & DNS-01), eigene Domains und vollständiges Request-Logging.

![Domains](assets/screenshots/domains.png)

</td>
<td width="50%">

**Request-Logs & Proxy-Einblick**
Jeder HTTP-Request wird mit Methode, Pfad, Status, Antwortzeit und Routing-Metadaten protokolliert. Filtern und suchen ohne zusätzliches Tooling.

![Proxy-Logs](assets/screenshots/proxy-logs.png)

</td>
</tr>
<tr>
<td width="100%" colspan="2">

**Managed Services & Transaktions-E-Mails**
Stelle Postgres, Redis, S3 (MinIO) und MongoDB direkt neben deinen Apps bereit — Temps kümmert sich um Erstellung, Backups und Abbau. Füge Absender-Domains mit DKIM-Records über die UI hinzu und versende Transaktions-E-Mails via `@temps-sdk/node-sdk`. Keine externen Dienste nötig.

</td>
</tr>
</table>

### Funktioniert mit deinem Stack

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

<p align="center"><em>Jede Sprache, jedes Framework. Automatisch erkannt — oder bring dein eigenes Dockerfile mit.</em></p>

---

## Schnellstart

```bash
curl -fsSL https://temps.sh/deploy.sh | bash
```

**Getestet auf:** Ubuntu 24.04 / 22.04 &nbsp;|&nbsp; Läuft auch auf macOS

Du willst keinen Server verwalten? [Temps Cloud](https://temps.sh/pricing) betreibt Temps für dich auf verwalteter Infrastruktur.

---

## Was Temps ersetzt

| Was du bekommst | Statt zu bezahlen für |
|---|---|
| Git-Deployments + Preview-URLs | Vercel / Netlify / Railway ($20+/Monat) |
| Web-Analytics + Funnels | PostHog / Plausible ($0-450/Monat) |
| Session Replay | PostHog / FullStory ($0-2000/Monat) |
| Error-Tracking | Sentry ($26+/Monat) |
| Uptime-Monitoring | Better Uptime / Pingdom ($20+/Monat) |
| Managed Postgres/Redis/S3 | AWS RDS / ElastiCache ($50+/Monat) |
| Transaktions-E-Mails + DKIM | Resend / SendGrid ($20-100/Monat) |
| Request-Logs + Proxy | Cloudflare ($0-200/Monat) |
| **Gesamt mit Temps** | **$0 (selbst gehostet)** |

---

## Temps vs. Alternativen

| Feature | Temps | Coolify | Dokploy | Kamal | Railway | Render | Vercel |
|---|:---:|:---:|:---:|:---:|:---:|:---:|:---:|
| Selbst gehostet & Open Source | Ja | Ja | Ja | Ja | Nein | Nein | Nein |
| Installation als einzelnes Binary | Ja | Nein | Nein | CLI-Tool | -- | -- | -- |
| Git-Push-Deploy | Ja | Ja | Ja | Nein | Ja | Ja | Ja |
| Preview-Deployments | Ja | Ja | Ja | Nein | Ja | Ja | Ja |
| Auto-TLS (HTTP-01 + DNS-01) | Ja | Ja | Ja | Ja | Ja | Ja | Ja |
| Docker-Compose-Unterstützung | Nein | Ja | Ja | Nein | -- | -- | -- |
| One-Click-Template-Bibliothek | Nein | 280+ | Ja | Nein | Ja | Ja | Ja |
| Web-Analytics | Ja | Nein | Nein | Nein | Nein | Nein | Kostenpflichtiges Add-on |
| Session Replay | Ja | Nein | Nein | Nein | Nein | Nein | Nein |
| Error-Tracking (Sentry-kompatibel) | Ja | Nein | Nein | Nein | Nein | Nein | Nein |
| Uptime-Monitoring | Ja | Nein | Nein | Nein | Nein | Nein | Nein |
| Transaktions-E-Mails + DKIM | Ja | Nein | Nein | Nein | Nein | Nein | Nein |
| Managed Postgres / Redis | Ja | Ja | Ja | Nein | Ja | Ja | Partner-Add-ons |
| S3-kompatibler Speicher | Ja | Nein | Nein | Nein | Nein | Nein | Blob (kostenpflichtig) |
| Multi-Node / Clustering | Ja | Ja | Swarm | Ja | Managed | Managed | Managed |
| Edge Functions / globales Edge-Netzwerk | Nein | Nein | Nein | Nein | Nein | Nein | Ja |
| Gebühren pro Nutzer | Nein | Nein | Nein | Nein | $20/Nutzer (Pro) | Pro Nutzer | $20/Seat (Pro) |

**Wo die Alternativen punkten.** Coolify und Dokploy bieten erstklassige Docker-Compose-Unterstützung und One-Click-Template-Bibliotheken (280+ Apps bei Coolify), die Temps noch nicht hat, und beide haben deutlich größere Communities — allein Coolify zählt über 56k GitHub-Sterne, während Temps das jüngste Projekt auf dieser Liste ist. Kamal ist die einfachere Wahl, wenn du nur Zero-Downtime-Docker-Deploys per CLI willst. Vercel und die anderen Managed-Plattformen bieten dir ein globales Edge-Netzwerk, Edge Functions und DDoS-Absorption, mit denen ein einzelner VPS nicht mithalten kann — und sie betreiben die Infrastruktur für dich, was echten Mehrwert bedeutet, wenn du dich nie um einen Server kümmern willst.

Ausführliche, regelmäßig aktualisierte Vergleiche: [temps.sh/compare](https://temps.sh/compare)

---

## Tech-Stack

- **Backend:** Rust, Axum, Sea-ORM, Pingora (Cloudflares Proxy-Engine), Bollard (Docker-API)
- **Frontend:** React 19, TypeScript, Tailwind CSS, shadcn/ui
- **Datenbank:** PostgreSQL + TimescaleDB
- **Architektur:** 30+ Workspace-Crates, dreischichtige Service-Architektur

---

## SDKs

| Paket | Beschreibung |
|---|---|
| [`@temps-sdk/node-sdk`](https://www.npmjs.com/package/@temps-sdk/node-sdk) | Platform-API-Client + Sentry-kompatibles Error-Tracking |
| [`@temps-sdk/react-analytics`](https://www.npmjs.com/package/@temps-sdk/react-analytics) | React-Analytics, Session Replay, Web Vitals, Engagement-Tracking |
| [`@temps-sdk/kv`](https://www.npmjs.com/package/@temps-sdk/kv) | Serverloser Key-Value-Store |
| [`@temps-sdk/blob`](https://www.npmjs.com/package/@temps-sdk/blob) | Dateispeicher (S3-kompatibel) |
| [`@temps-sdk/cli`](https://www.npmjs.com/package/@temps-sdk/cli) | Kommandozeilen-Interface |

<details>
<summary><strong>Kurze Beispiele</strong></summary>

**Analytics** -- umschließe deine React-App, alles Weitere passiert automatisch:

```tsx
import { TempsAnalyticsProvider } from '@temps-sdk/react-analytics';

export default function App({ children }) {
  return <TempsAnalyticsProvider>{children}</TempsAnalyticsProvider>;
}
```

**Error-Tracking** -- Sentry-kompatibel, Drop-in-Replacement:

```typescript
import { ErrorTracking } from '@temps-sdk/node-sdk';

ErrorTracking.init({ dsn: 'https://key@your-instance.temps.dev/1' });

try {
  riskyOperation();
} catch (error) {
  ErrorTracking.captureException(error);
}
```

**KV-Store** -- Redis-artige API, keine Konfiguration nötig:

```typescript
import { kv } from '@temps-sdk/kv';

await kv.set('user:123', { name: 'Alice', plan: 'pro' }, { ex: 3600 });
const user = await kv.get('user:123');
```

**Blob-Storage** -- Dateien hochladen und ausliefern:

```typescript
import { blob } from '@temps-sdk/blob';

const { url } = await blob.put('avatars/user-123.png', fileBuffer);
const files = await blob.list({ prefix: 'avatars/' });
```

</details>

---

## Community

- [GitHub Discussions](https://github.com/gotempsh/temps/discussions) — Fragen, Ideen und Show & Tell
- [GitHub Issues](https://github.com/gotempsh/temps/issues) — Bug-Reports und Feature-Requests

Wenn Temps dir eine SaaS-Rechnung erspart, hilft [ein Stern](https://github.com/gotempsh/temps) anderen, es zu finden.

---

## Star-Verlauf

<a href="https://www.star-history.com/#gotempsh/temps&Date">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/svg?repos=gotempsh/temps&type=Date&theme=dark" />
    <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/svg?repos=gotempsh/temps&type=Date" />
    <img alt="Star-History-Diagramm" src="https://api.star-history.com/svg?repos=gotempsh/temps&type=Date" />
  </picture>
</a>

---

## Mitwirken

Beiträge sind willkommen. Richtlinien findest du in [CONTRIBUTING.md](CONTRIBUTING.md).

```bash
git clone https://github.com/gotempsh/temps.git
cd temps
cargo build --release
```

---

## Lizenz

Doppelt lizenziert unter [MIT](LICENSE-MIT) oder [Apache 2.0](LICENSE).

---

<div align="center">

[temps.sh](https://temps.sh) | [Dokumentation](https://temps.sh/docs) | [GitHub](https://github.com/gotempsh/temps)

</div>
