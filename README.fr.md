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

<a href="assets/screenshots/dashboard-light.png">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/dashboard-dark.png">
    <img alt="Tableau de bord des projets Temps — services regroupés en projets, avec trafic, statut et dernier déploiement" src="assets/screenshots/dashboard-light.png">
  </picture>
</a><br>
<sub>Taille réelle : <a href="assets/screenshots/dashboard-light.png">clair</a> · <a href="assets/screenshots/dashboard-dark.png">sombre</a></sub>

## Fonctionnalités

<table>
  <tr>
    <td width="50%" valign="top">
      <b>Déploiement par git push</b><br>
      URL de prévisualisation et déploiements sans interruption pour tout langage, détectés automatiquement ou depuis votre Dockerfile ou fichier Docker Compose. <a href="https://temps.sh/docs/deployments">Docs</a><br><br>
      <a href="assets/screenshots/deployments-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/deployments-dark.png">
          <img alt="Déploiements Temps — versions de production et branches de prévisualisation avec commit, auteur et durée de build, et un build en échec avec sa cause" src="assets/screenshots/deployments-light.png">
        </picture>
      </a><br>
      <sub>Taille réelle : <a href="assets/screenshots/deployments-light.png">clair</a> · <a href="assets/screenshots/deployments-dark.png">sombre</a></sub>
    </td>
    <td width="50%" valign="top">
      <b>Bases de données gérées</b><br>
      PostgreSQL, Redis, MongoDB, MariaDB et stockage compatible S3, avec sauvegardes. <a href="https://temps.sh/docs/set-up-managed-services">Docs</a><br><br>
      <a href="assets/screenshots/databases-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/databases-dark.png">
          <img alt="Bases de données Temps — services PostgreSQL, Redis, MongoDB, MariaDB et compatibles S3 en cours d'exécution, avec CPU et mémoire en direct" src="assets/screenshots/databases-light.png">
        </picture>
      </a><br>
      <sub>Taille réelle : <a href="assets/screenshots/databases-light.png">clair</a> · <a href="assets/screenshots/databases-dark.png">sombre</a></sub>
    </td>
  </tr>
  <tr>
    <td width="50%" valign="top">
      <b>Domaines et TLS</b><br>
      Domaines personnalisés et wildcard avec certificats Let's Encrypt automatiques. <a href="https://temps.sh/docs/domains-ssl">Docs</a><br><br>
      <a href="assets/screenshots/domains-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/domains-dark.png">
          <img alt="Domaines Temps — chaque domaine avec sa méthode HTTPS et la date d'expiration du certificat" src="assets/screenshots/domains-light.png">
        </picture>
      </a><br>
      <sub>Taille réelle : <a href="assets/screenshots/domains-light.png">clair</a> · <a href="assets/screenshots/domains-dark.png">sombre</a></sub>
    </td>
    <td width="50%" valign="top">
      <b>Web analytics</b><br>
      Visiteurs, pages, sources, entonnoirs et performance web, collectés par votre propre serveur. <a href="https://temps.sh/docs/analytics">Docs</a><br><br>
      <a href="assets/screenshots/analytics-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/analytics-dark.png">
          <img alt="Web analytics Temps — visiteurs, sessions et trafic horaire" src="assets/screenshots/analytics-light.png">
        </picture>
      </a><br>
      <sub>Taille réelle : <a href="assets/screenshots/analytics-light.png">clair</a> · <a href="assets/screenshots/analytics-dark.png">sombre</a></sub>
    </td>
  </tr>
  <tr>
    <td width="50%" valign="top">
      <b>Session replay</b><br>
      Revoyez une visite telle qu'elle s'est déroulée, avec les champs de formulaire masqués par défaut. <a href="https://temps.sh/docs/session-replay">Docs</a><br><br>
      <a href="assets/screenshots/session-replay-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/session-replay-dark.png">
          <img alt="Session replay Temps — une visite enregistrée de la boutique au paiement, avec champs masqués et la chronologie des événements" src="assets/screenshots/session-replay-light.png">
        </picture>
      </a><br>
      <sub>Taille réelle : <a href="assets/screenshots/session-replay-light.png">clair</a> · <a href="assets/screenshots/session-replay-dark.png">sombre</a></sub>
    </td>
    <td width="50%" valign="top">
      <b>Suivi d'erreurs</b><br>
      Compatible Sentry : pointez votre SDK Sentry existant vers Temps, puis confiez une erreur à votre agent de code. <a href="https://temps.sh/docs/error-tracking">Docs</a><br><br>
      <a href="assets/screenshots/errors-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/errors-dark.png">
          <img alt="Suivi d'erreurs Temps — erreurs de tous les services avec statut, nombre d'événements et d'utilisateurs, et un bouton « Fix with agent »" src="assets/screenshots/errors-light.png">
        </picture>
      </a><br>
      <sub>Taille réelle : <a href="assets/screenshots/errors-light.png">clair</a> · <a href="assets/screenshots/errors-dark.png">sombre</a></sub>
    </td>
  </tr>
  <tr>
    <td width="50%" valign="top">
      <b>Traces distribuées</b><br>
      Ingestion OpenTelemetry, avec des traces qui suivent une requête entre services jusqu'au span en échec. <a href="https://temps.sh/docs/opentelemetry">Docs</a><br><br>
      <a href="assets/screenshots/traces-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/traces-dark.png">
          <img alt="Trace distribuée Temps — une requête de paiement à travers deux services, avec le span en échec et son erreur" src="assets/screenshots/traces-light.png">
        </picture>
      </a><br>
      <sub>Taille réelle : <a href="assets/screenshots/traces-light.png">clair</a> · <a href="assets/screenshots/traces-dark.png">sombre</a></sub>
    </td>
    <td width="50%" valign="top">
      <b>Logs</b><br>
      Logs de conteneurs et de requêtes de tous les services dans une seule vue avec recherche, avec le volume par niveau. <a href="https://temps.sh/docs/logs">Docs</a><br><br>
      <a href="assets/screenshots/logs-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/logs-dark.png">
          <img alt="Logs Temps — volume de logs par niveau, une liste avec recherche sur tous les services et des comptes par niveau et service" src="assets/screenshots/logs-light.png">
        </picture>
      </a><br>
      <sub>Taille réelle : <a href="assets/screenshots/logs-light.png">clair</a> · <a href="assets/screenshots/logs-dark.png">sombre</a></sub>
    </td>
  </tr>
  <tr>
    <td width="50%" valign="top">
      <b>Uptime et alertes</b><br>
      Health checks, pages de statut et règles d'alerte. <a href="https://temps.sh/docs/monitoring">Docs</a><br><br>
      <a href="assets/screenshots/uptime-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/uptime-dark.png">
          <img alt="Uptime Temps — statut actuel, pourcentage de disponibilité, temps de réponse moyen et une chronologie de statut horaire" src="assets/screenshots/uptime-light.png">
        </picture>
      </a><br>
      <sub>Taille réelle : <a href="assets/screenshots/uptime-light.png">clair</a> · <a href="assets/screenshots/uptime-dark.png">sombre</a></sub>
    </td>
    <td width="50%" valign="top">
      <b>Email transactionnel</b><br>
      Envoi via SMTP ou SES avec DKIM, et chaque message avec sa livraison, ses ouvertures et ses clics. <a href="https://temps.sh/docs/transactional-email">Docs</a><br><br>
      <a href="assets/screenshots/email-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/email-dark.png">
          <img alt="Email Temps — totaux envoyés et en échec, et messages récents avec leur statut, ouvertures et clics" src="assets/screenshots/email-light.png">
        </picture>
      </a><br>
      <sub>Taille réelle : <a href="assets/screenshots/email-light.png">clair</a> · <a href="assets/screenshots/email-dark.png">sombre</a></sub>
    </td>
  </tr>
  <tr>
    <td width="50%" valign="top">
      <b>Sandboxes IA</b><br>
      Sandboxes isolées Docker ou Firecracker pour exécuter du code, avec terminal et URL de prévisualisation. <a href="https://temps.sh/docs/sandboxes">Docs</a><br><br>
      <a href="assets/screenshots/sandbox-detail-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/sandbox-detail-dark.png">
          <img alt="Détail d'une sandbox Temps — une sandbox Docker en cours d'exécution avec un terminal en direct dans le navigateur, le temps restant et son modèle d'URL de prévisualisation" src="assets/screenshots/sandbox-detail-light.png">
        </picture>
      </a><br>
      <sub>Taille réelle : <a href="assets/screenshots/sandbox-detail-light.png">clair</a> · <a href="assets/screenshots/sandbox-detail-dark.png">sombre</a></sub>
    </td>
    <td width="50%" valign="top">
      <b>Passerelle IA</b><br>
      Un endpoint compatible OpenAI pour vos propres clés de fournisseur, avec usage et coût par modèle. <a href="https://temps.sh/docs/ai-gateway">Docs</a><br><br>
      <a href="assets/screenshots/ai-usage-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/ai-usage-dark.png">
          <img alt="Usage de la passerelle IA Temps — requêtes, tokens, latence, taux d'erreur et coût estimé par fournisseur" src="assets/screenshots/ai-usage-light.png">
        </picture>
      </a><br>
      <sub>Taille réelle : <a href="assets/screenshots/ai-usage-light.png">clair</a> · <a href="assets/screenshots/ai-usage-dark.png">sombre</a></sub>
    </td>
  </tr>
  <tr>
    <td width="50%" valign="top">
      <b>Chat IA</b><br>
      Posez une question sur un projet et obtenez des réponses tirées de ses propres erreurs, logs et traces. En lecture seule sauf si vous autorisez des modifications.<br><br>
      <a href="assets/screenshots/ai-chat-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/ai-chat-dark.png">
          <img alt="Le chat IA de Temps explique des paiements échoués à partir des erreurs, logs de requêtes et traces du projet, avec des correctifs suggérés" src="assets/screenshots/ai-chat-light.png">
        </picture>
      </a><br>
      <sub>Taille réelle : <a href="assets/screenshots/ai-chat-light.png">clair</a> · <a href="assets/screenshots/ai-chat-dark.png">sombre</a></sub>
    </td>
    <td width="50%" valign="top">
      <b>Import</b><br>
      Importez applications, bases de données avec leurs données, domaines et variables d'environnement depuis Kubernetes, Coolify, Dokploy, CapRover, Portainer, Kamal ou de simples conteneurs Docker, et depuis Vercel avec <code>temps migrate</code>. <a href="https://temps.sh/docs/import-existing-platform">Docs</a><br><br>
      <a href="assets/screenshots/import-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/import-dark.png">
          <img alt="Assistant d'import Temps — choisissez une source : Kubernetes, CapRover, Portainer, Kamal, Dokploy, Coolify ou conteneurs Docker" src="assets/screenshots/import-light.png">
        </picture>
      </a><br>
      <sub>Taille réelle : <a href="assets/screenshots/import-light.png">clair</a> · <a href="assets/screenshots/import-dark.png">sombre</a></sub>
    </td>
  </tr>
</table>

## Démarrer avec l'IA

Temps fournit des skills qui apprennent aux agents de code (Claude Code, Codex, Cursor, OpenCode et d'autres) à installer Temps sur un serveur, à s'y connecter et à exécuter toute opération de la console via la CLI. Ajoutez-les à votre agent :

```bash
bunx skills add gotempsh/temps --skill temps temps-platform-setup
```

**Pas encore de serveur ?** Donnez à votre agent un serveur Ubuntu tout neuf accessible en SSH et demandez-lui :

> Use the Temps skills to install Temps on root@203.0.113.10 in quick mode, connect the CLI to it, then deploy this repository and tell me its URL when it is live.

Il lance l'installateur officiel en SSH, laisse sur le serveur le mot de passe administrateur et la clé d'API générés, et vous donne un lien pour approuver la connexion de la CLI dans le navigateur : aucun identifiant ne passe par le chat.

**Temps tourne déjà ?** Connectez la CLI, puis demandez à votre agent :

```bash
bunx @temps-sdk/cli@0.1.38 login https://console.your-server.example
```

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
