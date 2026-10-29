<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="web/public/logo/temps-logo-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="web/public/logo/temps-logo-light.png">
  <img alt="Temps" src="web/public/logo/temps-logo-dark.png" width="280">
</picture>

**L'alternative open source à Vercel, Sentry, PostHog, Pingdom, Resend, E2B et Datadog, en un seul binaire auto-hébergé.**

[Site web](https://temps.sh/?utm_source=github&utm_medium=repo&utm_content=header_fr) · [Documentation](https://temps.sh/docs?utm_source=github&utm_medium=repo&utm_content=header_fr) · [Discussions](https://github.com/gotempsh/temps/discussions)

</div>

---

Temps est un binaire unique et auto-hébergé qui déploie vos applications et fait tourner tout ce qui les entoure : bases de données, analytics, suivi d'erreurs, traces, logs, uptime, email, sandboxes IA et une passerelle IA. Open source, sur votre propre serveur.

## Installation sur un VPS

```bash
curl -fsSL https://temps.sh/deploy.sh | bash
```

Lancez-le sur un serveur Ubuntu 24.04 ou 22.04 tout neuf. Il installe Docker s'il manque, fait tourner PostgreSQL, configure Temps comme service avec HTTPS automatique et affiche l'URL de la console et le mot de passe administrateur. Pas encore de domaine ? Choisissez le mode `quick` et vous obtenez `https://console.<ip-du-serveur>.sslip.io`.

[Démarrage rapide](https://temps.sh/docs/quickstart?utm_source=github&utm_medium=repo&utm_content=install_fr) · [Choisir un serveur](https://temps.sh/docs/choosing-a-server?utm_source=github&utm_medium=repo&utm_content=install_fr) · Vous préférez ne pas gérer de serveur ? [Temps Cloud](https://temps.sh/pricing?utm_source=github&utm_medium=repo&utm_content=cloud_cta_fr)

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/dashboard-dark.png">
  <img alt="Tableau de bord des projets Temps — services regroupés en projets, avec trafic, statut et dernier déploiement" src="assets/screenshots/dashboard-light.png">
</picture>

## Fonctionnalités

- **Déploiement par git push** — URLs de prévisualisation et déploiements sans interruption pour tout langage, détectés automatiquement ou depuis votre Dockerfile ou fichier Docker Compose. [Docs](https://temps.sh/docs/deployments)
- **Bases de données gérées** — PostgreSQL, Redis, MongoDB, MariaDB et stockage compatible S3, avec sauvegardes. [Docs](https://temps.sh/docs/set-up-managed-services)
- **Domaines & TLS** — domaines personnalisés avec certificats Let's Encrypt automatiques. [Docs](https://temps.sh/docs/domains-ssl)
- **Analytics web & session replay** — visiteurs, pages, entonnoirs et enregistrements, collectés par votre propre serveur. [Docs](https://temps.sh/docs/analytics)
- **Suivi d'erreurs** — compatible Sentry : pointez votre SDK Sentry existant vers Temps. [Docs](https://temps.sh/docs/error-tracking)
- **Traces, métriques & logs** — ingestion OpenTelemetry, traces entre services, logs de conteneurs et de requêtes consultables. [Docs](https://temps.sh/docs/opentelemetry)
- **Uptime & alertes** — vérifications de santé, pages de statut et règles d'alerte. [Docs](https://temps.sh/docs/monitoring)
- **Email transactionnel** — envoi via SMTP ou SES avec DKIM. [Docs](https://temps.sh/docs/transactional-email)
- **Sandboxes IA** — sandboxes isolés Docker ou Firecracker pour exécuter du code, avec terminal et URLs de prévisualisation. [Docs](https://temps.sh/docs/sandboxes)
- **Passerelle IA** — un endpoint compatible OpenAI pour vos propres clés de fournisseur, avec usage et coût par modèle. [Docs](https://temps.sh/docs/ai-gateway)
- **Chat IA** — posez une question sur un projet et obtenez des réponses tirées de ses propres erreurs, logs et traces. En lecture seule tant que vous n'autorisez pas de modifications.
- **Import** — récupérez applications, bases de données avec leurs données, domaines et variables d'environnement depuis Vercel, Netlify, Railway, Render, Fly.io, Coolify, Dokploy, CapRover, Portainer, Kamal ou Kubernetes. [Docs](https://temps.sh/docs/import-existing-platform)

<table>
  <tr>
    <td width="50%">
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/ai-chat-dark.png">
        <img alt="Le chat IA de Temps expliquant des checkouts échoués à partir des erreurs, logs de requêtes et traces du projet lui-même, avec des correctifs suggérés" src="assets/screenshots/ai-chat-light.png">
      </picture>
    </td>
    <td width="50%">
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/errors-dark.png">
        <img alt="Suivi d'erreurs Temps — erreurs de tous les services avec statut, nombre d'événements et d'utilisateurs, et un bouton « Fix with agent »" src="assets/screenshots/errors-light.png">
      </picture>
    </td>
  </tr>
  <tr>
    <td width="50%">
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/traces-dark.png">
        <img alt="Trace distribuée dans Temps — une requête de checkout à travers deux services, avec le span en échec et son erreur" src="assets/screenshots/traces-light.png">
      </picture>
    </td>
    <td width="50%">
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/logs-dark.png">
        <img alt="Logs Temps — volume de logs par niveau, une liste consultable de tous les services et des compteurs par niveau et par service" src="assets/screenshots/logs-light.png">
      </picture>
    </td>
  </tr>
  <tr>
    <td width="50%">
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/analytics-dark.png">
        <img alt="Analytics web Temps — visiteurs, sessions et trafic horaire" src="assets/screenshots/analytics-light.png">
      </picture>
    </td>
    <td width="50%">
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/sandbox-detail-dark.png">
        <img alt="Détail d'un sandbox Temps — un sandbox Docker en cours d'exécution avec un terminal en direct dans le navigateur, le temps restant et le modèle d'URL d'aperçu" src="assets/screenshots/sandbox-detail-light.png">
      </picture>
    </td>
  </tr>
</table>

## Démarrer avec l'IA

Chaque opération de la console est aussi une commande CLI, et Temps fournit un skill qui apprend aux agents de code (Claude Code, Codex, Cursor, OpenCode et d'autres) à s'en servir. Connectez le vôtre et laissez-le faire la configuration :

```bash
bunx skills add gotempsh/temps --skill temps
bunx @temps-sdk/cli login https://console.your-server.example
```

Puis demandez à votre agent :

> Use the Temps skill to verify my connection and list the services on this instance. Do not make any changes.

> Use the Temps skill to deploy this repository to Temps and tell me its URL when it is live.

Pour un agent qui tourne sans surveillance, ouvrez **Connect your coding agent** dans la console (`/setup/ai`) : la page crée une clé d'API dédiée et vérifie que la première requête de l'agent arrive.

[Skills](https://temps.sh/docs/skills) · [CLI](https://temps.sh/docs/cli) · [Serveurs MCP](https://temps.sh/docs/mcp-servers)

## Documentation

Tout le reste se trouve dans la documentation sur **[temps.sh/docs](https://temps.sh/docs?utm_source=github&utm_medium=repo&utm_content=docs_fr)**:

- [Votre premier déploiement](https://temps.sh/docs/your-first-deployment)
- [Importer depuis une autre plateforme](https://temps.sh/docs/import-existing-platform)
- [SDKs](https://temps.sh/docs/sdks) — analytics, suivi d'erreurs, stockage KV et blob
- [Checklist de mise en production](https://temps.sh/docs/production-checklist) et [mise à jour](https://temps.sh/docs/upgrade-temps)
- [Comment Temps se compare](https://temps.sh/compare?utm_source=github&utm_medium=repo&utm_content=compare_fr) à Vercel, Coolify, Dokploy et d'autres

## Télémétrie

Temps envoie des événements d'usage anonymes (un identifiant d'instance aléatoire, des noms d'événements comme `deploy_succeeded` et des compteurs) pour que les mainteneurs sachent si les installations auto-hébergées fonctionnent. Il n'envoie jamais d'emails, d'IPs, de noms de dépôts, de domaines ni ce que vous avez saisi, et les données de vos applications ne quittent jamais votre serveur. Désactivez-la dans **Settings › Telemetry**, ou pour tout un hôte avec `TEMPS_TELEMETRY=0`. [Détails](https://temps.sh/docs/data-ownership-and-privacy#anonymous-telemetry)

## Communauté & contributions

Questions et idées dans les [Discussions](https://github.com/gotempsh/temps/discussions), bugs dans les [Issues](https://github.com/gotempsh/temps/issues). Les contributions sont les bienvenues : voir [CONTRIBUTING.md](CONTRIBUTING.md).

## Licence

Double licence [MIT](LICENSE-MIT) ou [Apache 2.0](LICENSE). Les forks et œuvres dérivées sont les bienvenus, y compris les distributions renommées et commerciales, tant qu'ils conservent les mentions de copyright, de licence et d'attribution exigées par la licence choisie. Voir [NOTICE](NOTICE).

---

<div align="center">

[temps.sh](https://temps.sh/?utm_source=github&utm_medium=repo&utm_content=footer_fr) | [Documentation](https://temps.sh/docs?utm_source=github&utm_medium=repo&utm_content=footer_fr) | [GitHub](https://github.com/gotempsh/temps)

[English](README.md) | [简体中文](README.zh-CN.md) | [Español](README.es.md) | Français | [Deutsch](README.de.md) | [日本語](README.ja.md) | [Português](README.pt-BR.md)

</div>
