<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="web/public/logo/temps-logo-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="web/public/logo/temps-logo-light.png">
  <img alt="Temps" src="web/public/logo/temps-logo-dark.png" width="280">
</picture>

**Die Open-Source-Alternative zu Vercel, Sentry, PostHog, Pingdom, Resend, E2B und Datadog — in einem selbst gehosteten Binary.**

[Website](https://temps.sh/?utm_source=github&utm_medium=repo&utm_content=header_de) · [Dokumentation](https://temps.sh/docs?utm_source=github&utm_medium=repo&utm_content=header_de) · [Diskussionen](https://github.com/gotempsh/temps/discussions)

</div>

---

Temps ist ein einzelnes, selbst gehostetes Binary, das deine Apps deployt und alles drumherum betreibt: Datenbanken, Analytics, Error-Tracking, Traces, Logs, Uptime, E-Mail, KI-Sandboxes und ein KI-Gateway. Open Source, auf deinem eigenen Server.

## Installation auf einem VPS

```bash
curl -fsSL https://temps.sh/deploy.sh | bash
```

Führe es auf einem frischen Ubuntu-24.04- oder -22.04-Server aus. Es installiert Docker, falls es fehlt, betreibt PostgreSQL, richtet Temps als Dienst mit automatischem HTTPS ein und gibt die Konsolen-URL und das Admin-Passwort aus. Noch keine Domain? Wähle den Modus `quick` und du bekommst `https://console.<server-ip>.sslip.io`.

[Schnellstart](https://temps.sh/docs/quickstart?utm_source=github&utm_medium=repo&utm_content=install_de) · [Server wählen](https://temps.sh/docs/choosing-a-server?utm_source=github&utm_medium=repo&utm_content=install_de) · Lieber keinen Server betreiben? [Temps Cloud](https://temps.sh/pricing?utm_source=github&utm_medium=repo&utm_content=cloud_cta_de)

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/dashboard-dark.png">
  <img alt="Temps-Projekt-Dashboard — Services in Projekten gruppiert, mit Traffic, Status und letztem Deployment" src="assets/screenshots/dashboard-light.png">
</picture>

## Funktionen

- **Git-Push-Deployments** — Preview-URLs und Rollouts ohne Downtime für jede Sprache, automatisch erkannt oder aus deinem Dockerfile bzw. deiner Docker-Compose-Datei. [Doku](https://temps.sh/docs/deployments)
- **Verwaltete Datenbanken** — PostgreSQL, Redis, MongoDB, MariaDB und S3-kompatibler Speicher, mit Backups. [Doku](https://temps.sh/docs/set-up-managed-services)
- **Domains & TLS** — eigene Domains mit automatischen Let's-Encrypt-Zertifikaten. [Doku](https://temps.sh/docs/domains-ssl)
- **Web-Analytics & Session Replay** — Besucher, Seiten, Funnels und Replays, erfasst von deinem eigenen Server. [Doku](https://temps.sh/docs/analytics)
- **Error-Tracking** — Sentry-kompatibel: richte dein bestehendes Sentry-SDK auf Temps. [Doku](https://temps.sh/docs/error-tracking)
- **Traces, Metriken & Logs** — OpenTelemetry-Ingest, serviceübergreifende Traces, durchsuchbare Container- und Request-Logs. [Doku](https://temps.sh/docs/opentelemetry)
- **Uptime & Alerts** — Health-Checks, Statusseiten und Alert-Regeln. [Doku](https://temps.sh/docs/monitoring)
- **Transaktions-E-Mail** — Versand über SMTP oder SES mit DKIM. [Doku](https://temps.sh/docs/transactional-email)
- **KI-Sandboxes** — isolierte Docker- oder Firecracker-Sandboxes zum Ausführen von Code, mit Terminal und Vorschau-URLs. [Doku](https://temps.sh/docs/sandboxes)
- **KI-Gateway** — ein OpenAI-kompatibler Endpunkt für deine eigenen Provider-Keys, mit Nutzung und Kosten pro Modell. [Doku](https://temps.sh/docs/ai-gateway)
- **KI-Chat** — frag zu einem Projekt und erhalte Antworten aus seinen eigenen Fehlern, Logs und Traces. Nur lesend, solange du keine Änderungen erlaubst.
- **Import** — übernimm Apps, Datenbanken samt Daten, Domains und Umgebungsvariablen von Vercel, Netlify, Railway, Render, Fly.io, Coolify, Dokploy, CapRover, Portainer, Kamal oder Kubernetes. [Doku](https://temps.sh/docs/import-existing-platform)

<table>
  <tr>
    <td width="50%">
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/ai-chat-dark.png">
        <img alt="Der Temps-KI-Chat erklärt fehlgeschlagene Checkouts anhand der eigenen Fehler, Request-Logs und Traces des Projekts, mit Lösungsvorschlägen" src="assets/screenshots/ai-chat-light.png">
      </picture>
    </td>
    <td width="50%">
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/errors-dark.png">
        <img alt="Temps Error-Tracking — Fehler aller Services mit Status, Event- und Nutzerzahlen und einer Schaltfläche „Fix with agent“" src="assets/screenshots/errors-light.png">
      </picture>
    </td>
  </tr>
  <tr>
    <td width="50%">
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/traces-dark.png">
        <img alt="Verteilter Trace in Temps — ein Checkout-Request über zwei Services, mit dem fehlerhaften Span und seinem Fehler" src="assets/screenshots/traces-light.png">
      </picture>
    </td>
    <td width="50%">
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/logs-dark.png">
        <img alt="Temps-Logs — Log-Volumen nach Level, eine durchsuchbare Liste über alle Services und Zähler pro Level und Service" src="assets/screenshots/logs-light.png">
      </picture>
    </td>
  </tr>
  <tr>
    <td width="50%">
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/analytics-dark.png">
        <img alt="Temps-Web-Analytics — Besucher, Sessions und stündlicher Traffic" src="assets/screenshots/analytics-light.png">
      </picture>
    </td>
    <td width="50%">
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/sandbox-detail-dark.png">
        <img alt="Temps-Sandbox-Detail — eine laufende Docker-Sandbox mit Live-Terminal im Browser, Restlaufzeit und Vorschau-URL-Vorlage" src="assets/screenshots/sandbox-detail-light.png">
      </picture>
    </td>
  </tr>
</table>

## Mit KI loslegen

Jede Aktion der Konsole ist auch ein CLI-Befehl, und Temps liefert einen Skill mit, der Coding-Agents (Claude Code, Codex, Cursor, OpenCode und andere) beibringt, sie zu nutzen. Verbinde deinen Agent und lass ihn die Einrichtung übernehmen:

```bash
bunx skills add gotempsh/temps --skill temps
bunx @temps-sdk/cli login https://console.your-server.example
```

Dann frag deinen Agent:

> Use the Temps skill to verify my connection and list the services on this instance. Do not make any changes.

> Use the Temps skill to deploy this repository to Temps and tell me its URL when it is live.

Für einen Agent, der unbeaufsichtigt läuft, öffne in der Konsole **Connect your coding agent** (`/setup/ai`): Dort wird ein eigener API-Key erstellt und geprüft, ob die erste Anfrage des Agents ankommt.

[Skills](https://temps.sh/docs/skills) · [CLI](https://temps.sh/docs/cli) · [MCP-Server](https://temps.sh/docs/mcp-servers)

## Dokumentation

Alles Weitere steht in der Dokumentation unter **[temps.sh/docs](https://temps.sh/docs?utm_source=github&utm_medium=repo&utm_content=docs_de)**:

- [Dein erstes Deployment](https://temps.sh/docs/your-first-deployment)
- [Von einer anderen Plattform importieren](https://temps.sh/docs/import-existing-platform)
- [SDKs](https://temps.sh/docs/sdks) — Analytics, Error-Tracking, KV- und Blob-Speicher
- [Produktions-Checkliste](https://temps.sh/docs/production-checklist) und [Aktualisieren](https://temps.sh/docs/upgrade-temps)
- [Wie Temps sich vergleicht](https://temps.sh/compare?utm_source=github&utm_medium=repo&utm_content=compare_de) mit Vercel, Coolify, Dokploy und anderen

## Telemetrie

Temps sendet anonyme Nutzungsereignisse (eine zufällige Instanz-ID, Ereignisnamen wie `deploy_succeeded` und Zähler), damit die Maintainer sehen, ob selbst gehostete Installationen funktionieren. Es werden nie E-Mails, IPs, Repository-Namen, Domains oder Eingaben gesendet, und deine Anwendungsdaten verlassen nie deinen Server. Abschalten unter **Settings › Telemetry** oder für einen ganzen Host mit `TEMPS_TELEMETRY=0`. [Details](https://temps.sh/docs/data-ownership-and-privacy#anonymous-telemetry)

## Community & Mitwirken

Fragen und Ideen in den [Diskussionen](https://github.com/gotempsh/temps/discussions), Bugs in den [Issues](https://github.com/gotempsh/temps/issues). Beiträge sind willkommen: siehe [CONTRIBUTING.md](CONTRIBUTING.md).

## Lizenz

Doppelt lizenziert unter [MIT](LICENSE-MIT) oder [Apache 2.0](LICENSE). Forks und abgeleitete Werke sind willkommen, auch umbenannte und kommerzielle Distributionen, solange sie die Copyright-, Lizenz- und Namensnennungshinweise behalten, die die gewählte Lizenz verlangt. Siehe [NOTICE](NOTICE).

---

<div align="center">

[temps.sh](https://temps.sh/?utm_source=github&utm_medium=repo&utm_content=footer_de) | [Dokumentation](https://temps.sh/docs?utm_source=github&utm_medium=repo&utm_content=footer_de) | [GitHub](https://github.com/gotempsh/temps)

[English](README.md) | [简体中文](README.zh-CN.md) | [Español](README.es.md) | [Français](README.fr.md) | Deutsch | [日本語](README.ja.md) | [Português](README.pt-BR.md)

</div>
