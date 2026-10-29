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

<a href="assets/screenshots/dashboard-light.png">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/dashboard-dark.png">
    <img alt="Temps-Projekt-Dashboard — Services in Projekten gruppiert, mit Traffic, Status und letztem Deployment" src="assets/screenshots/dashboard-light.png">
  </picture>
</a><br>
<sub>Volle Größe: <a href="assets/screenshots/dashboard-light.png">hell</a> · <a href="assets/screenshots/dashboard-dark.png">dunkel</a></sub>

## Funktionen

<table>
  <tr>
    <td width="50%" valign="top">
      <b>Git-Push-Deployments</b><br>
      Preview-URLs und Rollouts ohne Downtime für jede Sprache, automatisch erkannt oder aus deinem Dockerfile bzw. deiner Docker-Compose-Datei. <a href="https://temps.sh/docs/deployments">Doku</a><br><br>
      <a href="assets/screenshots/deployments-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/deployments-dark.png">
          <img alt="Temps-Deployments — Produktions-Releases und Preview-Branches mit Commit, Autor und Build-Dauer, dazu ein fehlgeschlagener Build mit Ursache" src="assets/screenshots/deployments-light.png">
        </picture>
      </a><br>
      <sub>Volle Größe: <a href="assets/screenshots/deployments-light.png">hell</a> · <a href="assets/screenshots/deployments-dark.png">dunkel</a></sub>
    </td>
    <td width="50%" valign="top">
      <b>Verwaltete Datenbanken</b><br>
      PostgreSQL, Redis, MongoDB, MariaDB und S3-kompatibler Speicher, mit Backups. <a href="https://temps.sh/docs/set-up-managed-services">Doku</a><br><br>
      <a href="assets/screenshots/databases-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/databases-dark.png">
          <img alt="Temps-Datenbanken — laufende PostgreSQL-, Redis-, MongoDB-, MariaDB- und S3-kompatible Services mit Live-CPU und -Speicher" src="assets/screenshots/databases-light.png">
        </picture>
      </a><br>
      <sub>Volle Größe: <a href="assets/screenshots/databases-light.png">hell</a> · <a href="assets/screenshots/databases-dark.png">dunkel</a></sub>
    </td>
  </tr>
  <tr>
    <td width="50%" valign="top">
      <b>Domains & TLS</b><br>
      Eigene und Wildcard-Domains mit automatischen Let's-Encrypt-Zertifikaten. <a href="https://temps.sh/docs/domains-ssl">Doku</a><br><br>
      <a href="assets/screenshots/domains-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/domains-dark.png">
          <img alt="Temps-Domains — jede Domain mit ihrer HTTPS-Methode und dem Ablaufdatum des Zertifikats" src="assets/screenshots/domains-light.png">
        </picture>
      </a><br>
      <sub>Volle Größe: <a href="assets/screenshots/domains-light.png">hell</a> · <a href="assets/screenshots/domains-dark.png">dunkel</a></sub>
    </td>
    <td width="50%" valign="top">
      <b>Web-Analytics</b><br>
      Besucher, Seiten, Quellen, Funnels und Web-Performance, erfasst von deinem eigenen Server. <a href="https://temps.sh/docs/analytics">Doku</a><br><br>
      <a href="assets/screenshots/analytics-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/analytics-dark.png">
          <img alt="Temps-Web-Analytics — Besucher, Sessions und stündlicher Traffic" src="assets/screenshots/analytics-light.png">
        </picture>
      </a><br>
      <sub>Volle Größe: <a href="assets/screenshots/analytics-light.png">hell</a> · <a href="assets/screenshots/analytics-dark.png">dunkel</a></sub>
    </td>
  </tr>
  <tr>
    <td width="50%" valign="top">
      <b>Session Replay</b><br>
      Sieh dir einen Besuch so an, wie er passiert ist, mit standardmäßig maskierten Formularfeldern. <a href="https://temps.sh/docs/session-replay">Doku</a><br><br>
      <a href="assets/screenshots/session-replay-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/session-replay-dark.png">
          <img alt="Temps Session Replay — ein aufgezeichneter Shop-Besuch im Checkout, mit maskierten Eingaben und der Ereignis-Zeitleiste" src="assets/screenshots/session-replay-light.png">
        </picture>
      </a><br>
      <sub>Volle Größe: <a href="assets/screenshots/session-replay-light.png">hell</a> · <a href="assets/screenshots/session-replay-dark.png">dunkel</a></sub>
    </td>
    <td width="50%" valign="top">
      <b>Error-Tracking</b><br>
      Sentry-kompatibel: richte dein bestehendes Sentry-SDK auf Temps und übergib einen Fehler an deinen Coding-Agent. <a href="https://temps.sh/docs/error-tracking">Doku</a><br><br>
      <a href="assets/screenshots/errors-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/errors-dark.png">
          <img alt="Temps Error-Tracking — Fehler aller Services mit Status, Event- und Nutzerzahlen und einer Schaltfläche „Fix with agent“" src="assets/screenshots/errors-light.png">
        </picture>
      </a><br>
      <sub>Volle Größe: <a href="assets/screenshots/errors-light.png">hell</a> · <a href="assets/screenshots/errors-dark.png">dunkel</a></sub>
    </td>
  </tr>
  <tr>
    <td width="50%" valign="top">
      <b>Verteilte Traces</b><br>
      OpenTelemetry-Ingest, mit Traces, die einen Request über Services hinweg bis zum fehlerhaften Span verfolgen. <a href="https://temps.sh/docs/opentelemetry">Doku</a><br><br>
      <a href="assets/screenshots/traces-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/traces-dark.png">
          <img alt="Verteilter Trace in Temps — ein Checkout-Request über zwei Services, mit dem fehlerhaften Span und seinem Fehler" src="assets/screenshots/traces-light.png">
        </picture>
      </a><br>
      <sub>Volle Größe: <a href="assets/screenshots/traces-light.png">hell</a> · <a href="assets/screenshots/traces-dark.png">dunkel</a></sub>
    </td>
    <td width="50%" valign="top">
      <b>Logs</b><br>
      Container- und Request-Logs aller Services in einer durchsuchbaren Ansicht, mit Volumen nach Level. <a href="https://temps.sh/docs/logs">Doku</a><br><br>
      <a href="assets/screenshots/logs-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/logs-dark.png">
          <img alt="Temps-Logs — Log-Volumen nach Level, eine durchsuchbare Liste über alle Services und Zähler pro Level und Service" src="assets/screenshots/logs-light.png">
        </picture>
      </a><br>
      <sub>Volle Größe: <a href="assets/screenshots/logs-light.png">hell</a> · <a href="assets/screenshots/logs-dark.png">dunkel</a></sub>
    </td>
  </tr>
  <tr>
    <td width="50%" valign="top">
      <b>Uptime & Alerts</b><br>
      Health-Checks, Statusseiten und Alert-Regeln. <a href="https://temps.sh/docs/monitoring">Doku</a><br><br>
      <a href="assets/screenshots/uptime-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/uptime-dark.png">
          <img alt="Temps-Uptime — aktueller Status, Verfügbarkeit in Prozent, mittlere Antwortzeit und eine stündliche Status-Zeitleiste" src="assets/screenshots/uptime-light.png">
        </picture>
      </a><br>
      <sub>Volle Größe: <a href="assets/screenshots/uptime-light.png">hell</a> · <a href="assets/screenshots/uptime-dark.png">dunkel</a></sub>
    </td>
    <td width="50%" valign="top">
      <b>Transaktions-E-Mail</b><br>
      Versand über SMTP oder SES mit DKIM, und jede Nachricht mit Zustellung, Öffnungen und Klicks. <a href="https://temps.sh/docs/transactional-email">Doku</a><br><br>
      <a href="assets/screenshots/email-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/email-dark.png">
          <img alt="Temps-E-Mail — Summen gesendeter und fehlgeschlagener Mails und aktuelle Nachrichten mit Status, Öffnungen und Klicks" src="assets/screenshots/email-light.png">
        </picture>
      </a><br>
      <sub>Volle Größe: <a href="assets/screenshots/email-light.png">hell</a> · <a href="assets/screenshots/email-dark.png">dunkel</a></sub>
    </td>
  </tr>
  <tr>
    <td width="50%" valign="top">
      <b>KI-Sandboxes</b><br>
      Isolierte Docker- oder Firecracker-Sandboxes zum Ausführen von Code, mit Terminal und Vorschau-URLs. <a href="https://temps.sh/docs/sandboxes">Doku</a><br><br>
      <a href="assets/screenshots/sandbox-detail-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/sandbox-detail-dark.png">
          <img alt="Temps-Sandbox-Detail — eine laufende Docker-Sandbox mit Live-Terminal im Browser, Restlaufzeit und Vorschau-URL-Vorlage" src="assets/screenshots/sandbox-detail-light.png">
        </picture>
      </a><br>
      <sub>Volle Größe: <a href="assets/screenshots/sandbox-detail-light.png">hell</a> · <a href="assets/screenshots/sandbox-detail-dark.png">dunkel</a></sub>
    </td>
    <td width="50%" valign="top">
      <b>KI-Gateway</b><br>
      Ein OpenAI-kompatibler Endpunkt für deine eigenen Provider-Keys, mit Nutzung und Kosten pro Modell. <a href="https://temps.sh/docs/ai-gateway">Doku</a><br><br>
      <a href="assets/screenshots/ai-usage-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/ai-usage-dark.png">
          <img alt="Temps-KI-Gateway-Nutzung — Requests, Tokens, Latenz, Fehlerrate und geschätzte Kosten über alle Provider" src="assets/screenshots/ai-usage-light.png">
        </picture>
      </a><br>
      <sub>Volle Größe: <a href="assets/screenshots/ai-usage-light.png">hell</a> · <a href="assets/screenshots/ai-usage-dark.png">dunkel</a></sub>
    </td>
  </tr>
  <tr>
    <td width="50%" valign="top">
      <b>KI-Chat</b><br>
      Frag zu einem Projekt und erhalte Antworten aus seinen eigenen Fehlern, Logs und Traces. Nur lesend, solange du keine Änderungen erlaubst.<br><br>
      <a href="assets/screenshots/ai-chat-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/ai-chat-dark.png">
          <img alt="Der Temps-KI-Chat erklärt fehlgeschlagene Checkouts anhand der eigenen Fehler, Request-Logs und Traces des Projekts, mit Lösungsvorschlägen" src="assets/screenshots/ai-chat-light.png">
        </picture>
      </a><br>
      <sub>Volle Größe: <a href="assets/screenshots/ai-chat-light.png">hell</a> · <a href="assets/screenshots/ai-chat-dark.png">dunkel</a></sub>
    </td>
    <td width="50%" valign="top">
      <b>Import</b><br>
      Übernimm Apps, Datenbanken samt Daten, Domains und Umgebungsvariablen von Kubernetes, Coolify, Dokploy, CapRover, Portainer, Kamal oder einfachen Docker-Containern, und von Vercel mit <code>temps migrate</code>. <a href="https://temps.sh/docs/import-existing-platform">Doku</a><br><br>
      <a href="assets/screenshots/import-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/import-dark.png">
          <img alt="Temps-Import-Assistent — Quelle wählen: Kubernetes, CapRover, Portainer, Kamal, Dokploy, Coolify oder Docker-Container" src="assets/screenshots/import-light.png">
        </picture>
      </a><br>
      <sub>Volle Größe: <a href="assets/screenshots/import-light.png">hell</a> · <a href="assets/screenshots/import-dark.png">dunkel</a></sub>
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
