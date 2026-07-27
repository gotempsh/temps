<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="web/public/logo/temps-logo-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="web/public/logo/temps-logo-light.png">
  <img alt="Temps" src="web/public/logo/temps-logo-dark.png" width="280">
</picture>

**Die Open-Source-Alternative zu Vercel + Sentry + PostHog + Pingdom + Resend + E2B.**
Deployments, Analytics, Session Replay, Error Tracking, Uptime-Monitoring, Transaktions-E-Mails & KI-Sandboxes -- in einem selbst gehosteten Binary.

**KI-nativ:** 440+ CLI-Operationen und Skills, die direkt in Claude Code, Codex und OpenCode passen.

[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE)
[![GitHub Release](https://img.shields.io/github/v/release/gotempsh/temps)](https://github.com/gotempsh/temps/releases)
[![Rust](https://img.shields.io/badge/rust-1.70%2B-orange.svg)](https://www.rust-lang.org/)
[![GitHub Stars](https://img.shields.io/github/stars/gotempsh/temps?style=social)](https://github.com/gotempsh/temps)

[Website](https://temps.sh) · [Dokumentation](https://temps.sh/docs) · [Schnellstart](https://temps.sh/docs/introduction) · [Diskussionen](https://github.com/gotempsh/temps/discussions) · [Contributing](CONTRIBUTING.md)

[English](README.md) | [简体中文](README.zh-CN.md) | [Español](README.es.md) | [Français](README.fr.md) | Deutsch | [日本語](README.ja.md) | [Português](README.pt-BR.md)

</div>

---

```bash
curl -fsSL https://temps.sh/deploy.sh | bash
```

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/create-dark.png">
  <img alt="Import eines öffentlichen Repositories in Temps — Framework-Presets werden vor dem Deploy automatisch erkannt" src="assets/screenshots/create-light.png">
</picture>


Hör auf, für 7 verschiedene SaaS-Tools zu bezahlen. Temps ersetzt deine Deployment-Plattform, Analytics, Error-Tracking, Session Replay, Uptime-Monitoring, Transaktions-E-Mails und KI-Code-Sandboxes -- alles selbst gehostet, alles in einem Binary.

---

## Features

### KI-nativ — 440+ Operationen, die ein Agent ausführen kann

Jede Operation im Dashboard ist auch ein CLI-Befehl — **440+ in 69 Gruppen** — und Temps liefert die [Skills](skills/) mit, die einem Agenten beibringen, sie zu benutzen. Leg sie in **Claude Code**, **Codex**, **OpenCode** oder jedes Harness, das `.claude/skills/` liest, und dein Agent kann deployen, Traces prüfen, Migrationen fahren oder eine Domain hinzufügen, ohne dass du den Klebstoff schreibst.

```bash
bunx @temps-sdk/cli projects list
bunx @temps-sdk/cli deploy my-app --environment production
bunx @temps-sdk/cli analytics ai-agents -p my-app --period 7d
```

Temps führt diese Agenten auch für dich aus: Workflow-Sandboxes starten Claude Code, Codex oder OpenCode gegen dein Repository — plattformweite Skills und MCP-Server werden automatisch injiziert.

### AI Chat — verankert in deiner eigenen Telemetrie

Frag nach deinem Projekt und die Antwort kommt aus deinen Daten — Traces, Metriken, Alarme, Deployments und Umsatz — nicht aus der Vermutung eines generischen Modells. Standardmäßig **read-only**: Schreibaktionen sind opt-in, und selbst dann schlägt der Assistent die Änderung vor und wartet auf deine Bestätigung.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/ai-chat-dark.png">
  <img alt="Der Temps-KI-Chat diagnostiziert einen Checkout-Latenzanstieg anhand der eigenen Traces, Metriken und Umsatzdaten des Projekts" src="assets/screenshots/ai-chat-light.png">
</picture>

### AI Gateway — ein Endpunkt, deine eigenen Keys

Bring deine eigenen Provider-Keys mit (OpenAI, Anthropic, xAI, Google Gemini) und rufe sie alle über einen OpenAI-kompatiblen Endpunkt auf — Base-URL tauschen, SDK behalten. Die Keys bleiben verschlüsselt auf deinem Server, und jeder Request wird zugeordnet: Tokens, Latenz, Fehlerrate und geschätzte Kosten pro Modell.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/ai-gateway-dark.png">
  <img alt="Temps AI Gateway — eigene Provider-Keys (BYOK) hinter einem OpenAI-kompatiblen Endpunkt" src="assets/screenshots/ai-gateway-light.png">
</picture>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/ai-usage-dark.png">
  <img alt="Nutzungsanalyse des Temps AI Gateway — Requests, Tokens, Latenz, Fehlerrate und geschätzte Kosten" src="assets/screenshots/ai-usage-light.png">
</picture>

### Web-Analytics & Session Replay

Web-Analytics mit Funnels, Besucher-Tracking und Session Replay (rrweb) direkt eingebaut — keine externen Dienste, keine Daten, die deine Server verlassen. Das bietet keine andere selbst gehostete PaaS.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/analytics-dark.png">
  <img alt="Temps Web-Analytics — Besucher, Sessions, Seiten, Funnels" src="assets/screenshots/analytics-light.png">
</picture>

### Uptime-Monitoring & Alerts

Uptime-Monitore mit Status-Zeitleisten, dazu Alerts bei fehlgeschlagenen Deployments, Laufzeit-Abstürzen, ablaufenden Zertifikaten und Backup-Problemen. Erfahre von Problemen, bevor deine Nutzer sie bemerken.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/uptime-dark.png">
  <img alt="Temps Uptime-Monitoring — Status-Zeitleiste, Verfügbarkeit in Prozent, Antwortzeit" src="assets/screenshots/uptime-light.png">
</picture>

### Error-Tracking — Sentry-kompatibel

Drop-in-Ersatz für Sentry: Richte das offizielle Sentry-SDK auf deinen Temps-DSN und erhalte Fehlergruppen, Stacktraces mit Quellkontext und Alerts. Keine Abrechnung pro Event.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/errors-dark.png">
  <img alt="Temps Error-Tracking — Fehlergruppen mit Events und Zeitleisten" src="assets/screenshots/errors-light.png">
</picture>

### Request-Logs & Proxy-Einblick

Jeder HTTP-Request wird mit Methode, Pfad, Status, Antwortzeit und Routing-Metadaten protokolliert — inklusive Traffic pro AI-Crawler (OpenAI, Anthropic, Perplexity, Google…). Läuft auf Cloudflares Pingora-Engine mit automatischem TLS via Let's Encrypt (HTTP-01 & DNS-01).

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/request-logs-dark.png">
  <img alt="Temps Proxy-Request-Logs mit Filterung nach AI-Agenten" src="assets/screenshots/request-logs-light.png">
</picture>

### Transaktions-E-Mails

Füge Absender-Domains mit DKIM-Records über die UI hinzu und versende via `@temps-sdk/node-sdk` — oder binde AWS SES, Scaleway oder ein beliebiges SMTP-Relay an.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/email-dark.png">
  <img alt="Temps E-Mail-Provider — SMTP, Scaleway und AWS SES" src="assets/screenshots/email-light.png">
</picture>

### OpenTelemetry — Traces, Metriken, Logs & Alerts

Richte einen beliebigen OTLP-Exporter auf Temps und du bekommst verteilte Traces, Metriken und strukturierte Logs am selben Ort wie alles andere. Traces zeigen Latenz und Fehler pro Span über Services hinweg, Metriken halten deine Golden Signals fest, und Alerts feuern auf Basis dieser Metriken in eine einzige Queue, die du bestätigen oder auflösen kannst. Kein Grafana, Prometheus, Jaeger oder Loki zu betreiben.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/traces-dark.png">
  <img alt="Verteilte Traces in Temps — Latenz pro Request, Span-Anzahl und Fehler über Services hinweg" src="assets/screenshots/traces-light.png">
</picture>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/metrics-dark.png">
  <img alt="OpenTelemetry-Metriken in Temps — Request-Rate, Latenz, Datenbank- und Cache-Signale" src="assets/screenshots/metrics-light.png">
</picture>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/otel-logs-dark.png">
  <img alt="Strukturierte Logs in Temps — Severity, Service und Nachricht, korreliert mit Traces" src="assets/screenshots/otel-logs-light.png">
</picture>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/alerts-dark.png">
  <img alt="Temps-Alerts — feuernde, bestätigte und aufgelöste Alarme aus Metriken, Containern, Uptime und Datenbanken" src="assets/screenshots/alerts-light.png">
</picture>

### KI-Sandboxes — Firecracker-microVMs, selbst gehostet

**Echte Isolation auf Hardware-Ebene, nicht nur Container.** Sandboxes laufen auf **Firecracker-microVMs** — derselben Technik hinter AWS Lambda — mit **Docker** als Standard-Backend. Führe `temps firecracker setup` aus, und Temps leitet Sandboxes automatisch auf microVMs; jede bekommt ihren eigenen Kernel, sodass von Agenten generierter Code nie den Kernel deines Hosts teilt.

**Ein Drop-in-SDK.** `@temps-sdk/sandbox` ist kompatibel zur Form von `@vercel/sandbox` — Anbieter wechseln heißt Import und Base-URL ändern:

```ts
import { Sandbox } from '@temps-sdk/sandbox'

const sandbox = await Sandbox.create({
  source: { type: 'git', url: 'https://github.com/example/repo.git', revision: 'main' },
})

const { stdout } = await sandbox.exec(['npm', 'test'])
const url = sandbox.domain(3000) // Live-Vorschau eines Dev-Servers in der VM
```

**Passwortgeschützte Vorschauen.** Jeder Sandbox-Port lässt sich über eine öffentliche Vorschau-URL bereitstellen und mit einem generierten Passwort sperren:

```bash
bunx @temps-sdk/cli sandbox password sbx_abc123 --rotate --length 32
bunx @temps-sdk/cli sandbox password sbx_abc123 --clear   # wieder öffnen
```

Teile einen laufenden Branch, ohne ihn der ganzen Welt auszuliefern.

Ebenfalls per CLI und REST-API verfügbar. Genau das, wofür du sonst E2B, Daytona oder Vercel Sandbox bezahlst.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/sandboxes-dark.png">
  <img alt="Temps-Sandboxes — laufende Sandboxes mit kopierfertigen CLI-, REST- und SDK-Snippets" src="assets/screenshots/sandboxes-light.png">
</picture>

Jede Sandbox bekommt eine Shell, eine Vorschau-URL-Vorlage für jeden gebundenen Port und eine Chronik von allem, was mit ihr passiert ist:

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/sandbox-detail-dark.png">
  <img alt="Temps-Sandbox-Detail — Docker-/Firecracker-Backend, Befehlsausführung im Browser, Vorschau-URL-Vorlage und passwortgeschützte Vorschauen" src="assets/screenshots/sandbox-detail-light.png">
</picture>

### Alles in einem Dashboard

Besucher, Fehler, Deployment-Status und Monitoring-Zustand pro Projekt — an einem Ort statt in sechs Browser-Tabs.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/dashboard-dark.png">
  <img alt="Temps Projekt-Dashboard — alle Projekte mit Besuchern und Status" src="assets/screenshots/dashboard-light.png">
</picture>

### Git Push to Deploy & Managed Services

Push nach Git und Temps baut, deployt und erstellt Preview-URLs mit Zero-Downtime-Rollouts — jede Sprache, automatisch erkannt. Stelle Postgres, Redis, S3 (MinIO) und MongoDB direkt neben deinen Apps bereit; Erstellung, Backups und Abbau übernimmt Temps für dich.

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

## Läuft schon woanders?

Temps importiert deine bestehende Einrichtung, statt dich zum Neuaufbau zu zwingen. Richte den Assistenten auf deine aktuelle Plattform, und er übernimmt alles — Apps, Datenbanken *samt Daten*, Domains und Umgebungsvariablen.

**Selbstgehostete Plattformen**

<p align="center">
<a href="https://coolify.io"><img src="https://img.shields.io/badge/Coolify-8B5CF6?logo=coolify&logoColor=fff&style=for-the-badge" alt="Coolify" /></a>
<a href="https://dokploy.com"><img src="https://img.shields.io/badge/Dokploy-0B0B0B?style=for-the-badge" alt="Dokploy" /></a>
<a href="https://caprover.com"><img src="https://img.shields.io/badge/CapRover-2196F3?logo=caprover&logoColor=fff&style=for-the-badge" alt="CapRover" /></a>
<a href="https://portainer.io"><img src="https://img.shields.io/badge/Portainer-13BEF9?logo=portainer&logoColor=fff&style=for-the-badge" alt="Portainer" /></a>
<a href="https://kamal-deploy.org"><img src="https://img.shields.io/badge/Kamal-1B1B1B?style=for-the-badge" alt="Kamal" /></a>
<a href="https://kubernetes.io"><img src="https://img.shields.io/badge/Kubernetes-326CE5?logo=kubernetes&logoColor=fff&style=for-the-badge" alt="Kubernetes" /></a>
<a href="https://docker.com"><img src="https://img.shields.io/badge/Docker-2496ED?logo=docker&logoColor=fff&style=for-the-badge" alt="Docker" /></a>
</p>

**Gehostete Plattformen**

<p align="center">
<a href="https://vercel.com"><img src="https://img.shields.io/badge/Vercel-000?logo=vercel&logoColor=fff&style=for-the-badge" alt="Vercel" /></a>
<a href="https://netlify.com"><img src="https://img.shields.io/badge/Netlify-00C7B7?logo=netlify&logoColor=fff&style=for-the-badge" alt="Netlify" /></a>
<a href="https://railway.app"><img src="https://img.shields.io/badge/Railway-0B0D0E?logo=railway&logoColor=fff&style=for-the-badge" alt="Railway" /></a>
<a href="https://render.com"><img src="https://img.shields.io/badge/Render-000?logo=render&logoColor=fff&style=for-the-badge" alt="Render" /></a>
<a href="https://fly.io"><img src="https://img.shields.io/badge/Fly.io-24175B?logo=flydotio&logoColor=fff&style=for-the-badge" alt="Fly.io" /></a>
</p>

<p align="center"><em>Der Import läuft über das Dashboard — die Plattform-Kacheln findest du direkt im Kopfbereich deiner Projektseite.</em></p>

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
| Traces, Metriken & Logs (OpenTelemetry) | Grafana Cloud / Datadog ($0-500+/Monat) |
| Uptime-Monitoring | Better Uptime / Pingdom ($20+/Monat) |
| Managed Postgres/Redis/S3 | AWS RDS / ElastiCache ($50+/Monat) |
| Transaktions-E-Mails + DKIM | Resend / SendGrid ($20-100/Monat) |
| KI-Code-Sandboxes | E2B / Daytona / Vercel Sandbox ($150+/Monat + Nutzung) |
| AI Gateway + Nutzungs-/Kostenerfassung | OpenRouter / Helicone / LangSmith ($0-200+/Monat) |
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
| Docker-Compose-Unterstützung | Ja | Ja | Ja | Nein | -- | -- | -- |
| One-Click-Template-Bibliothek | Nein | 280+ | Ja | Nein | Ja | Ja | Ja |
| Web-Analytics | Ja | Nein | Nein | Nein | Nein | Nein | Kostenpflichtiges Add-on |
| Session Replay | Ja | Nein | Nein | Nein | Nein | Nein | Nein |
| Error-Tracking (Sentry-kompatibel) | Ja | Nein | Nein | Nein | Nein | Nein | Nein |
| OpenTelemetry Traces + Metriken + Logs | Ja | Nein | Nein | Nein | Nein | Nein | Traces (kostenpflichtig) |
| Uptime-Monitoring | Ja | Nein | Nein | Nein | Nein | Nein | Nein |
| Transaktions-E-Mails + DKIM | Ja | Nein | Nein | Nein | Nein | Nein | Nein |
| Code-Sandboxes (API) | Ja | Nein | Nein | Nein | Nein | Nein | Sandbox (nutzungsbasiert) |
| AI Gateway (BYOK) + Assistent | Ja | Nein | Nein | Nein | Nein | Nein | AI Gateway (kostenpflichtig) |
| Managed Postgres / Redis | Ja | Ja | Ja | Nein | Ja | Ja | Partner-Add-ons |
| S3-kompatibler Speicher | Ja | Nein | Nein | Nein | Nein | Nein | Blob (kostenpflichtig) |
| Multi-Node / Clustering | Ja | Ja | Swarm | Ja | Managed | Managed | Managed |
| Edge Functions / globales Edge-Netzwerk | Nein | Nein | Nein | Nein | Nein | Nein | Ja |
| Gebühren pro Nutzer | Nein | Nein | Nein | Nein | $20/Nutzer (Pro) | Pro Nutzer | $20/Seat (Pro) |

**Wo die Alternativen punkten.** Coolify und Dokploy bieten One-Click-Template-Bibliotheken (280+ Apps bei Coolify), die Temps noch nicht hat, und beide haben deutlich größere Communities — allein Coolify zählt über 56k GitHub-Sterne, während Temps das jüngste Projekt auf dieser Liste ist. Kamal ist die einfachere Wahl, wenn du nur Zero-Downtime-Docker-Deploys per CLI willst. Vercel und die anderen Managed-Plattformen bieten dir ein globales Edge-Netzwerk, Edge Functions und DDoS-Absorption, mit denen ein einzelner VPS nicht mithalten kann — und sie betreiben die Infrastruktur für dich, was echten Mehrwert bedeutet, wenn du dich nie um einen Server kümmern willst.

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
