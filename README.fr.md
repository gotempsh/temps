<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="web/public/logo/temps-logo-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="web/public/logo/temps-logo-light.png">
  <img alt="Temps" src="web/public/logo/temps-logo-dark.png" width="280">
</picture>

**L'alternative open source à Vercel + Sentry + PostHog + Pingdom + Resend + E2B.**
Déploiements, analytics, session replay, suivi d'erreurs, monitoring de disponibilité, emails transactionnels et sandboxes IA -- en un seul binaire auto-hébergé.

**Nativement IA :** plus de 440 opérations CLI et des skills prêtes pour Claude Code, Codex et OpenCode.

[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE)
[![GitHub Release](https://img.shields.io/github/v/release/gotempsh/temps)](https://github.com/gotempsh/temps/releases)
[![Rust](https://img.shields.io/badge/rust-1.70%2B-orange.svg)](https://www.rust-lang.org/)
[![GitHub Stars](https://img.shields.io/github/stars/gotempsh/temps?style=social)](https://github.com/gotempsh/temps)

[Site web](https://temps.sh/?utm_source=github&utm_medium=repo&utm_content=header_fr) · [Documentation](https://temps.sh/docs?utm_source=github&utm_medium=repo&utm_content=header_fr) · [Démarrage rapide](https://temps.sh/docs/introduction?utm_source=github&utm_medium=repo&utm_content=header_fr) · [Discussions](https://github.com/gotempsh/temps/discussions) · [Contributing](CONTRIBUTING.md)

[English](README.md) | [简体中文](README.zh-CN.md) | [Español](README.es.md) | Français | [Deutsch](README.de.md) | [日本語](README.ja.md) | [Português](README.pt-BR.md)

</div>

---

```bash
curl -fsSL https://temps.sh/deploy.sh | bash
```

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/create-dark.png">
  <img alt="Import d'un dépôt public dans Temps — les presets de framework sont détectés automatiquement avant le déploiement" src="assets/screenshots/create-light.png">
</picture>


Arrêtez de payer 7 outils SaaS différents. Temps remplace votre plateforme de déploiement, vos analytics, votre suivi d'erreurs, votre session replay, votre monitoring de disponibilité, vos emails transactionnels et vos sandboxes d'exécution de code pour l'IA -- le tout auto-hébergé, le tout dans un seul binaire.

---

## Fonctionnalités

### Nativement IA — plus de 440 opérations pilotables par un agent

Chaque opération du tableau de bord est aussi une commande CLI — **plus de 440 réparties en 69 groupes** — et Temps fournit les [skills](skills/) qui apprennent à un agent à s'en servir. Placez-les dans **Claude Code**, **Codex**, **OpenCode** ou tout harness lisant `.claude/skills/`, et votre agent peut déployer, inspecter des traces, lancer des migrations ou ajouter un domaine sans que vous écriviez la glue.

```bash
bunx @temps-sdk/cli projects list
bunx @temps-sdk/cli deploy my-app --environment production
bunx @temps-sdk/cli analytics ai-agents -p my-app --period 7d
```

Temps exécute aussi ces agents pour vous : les sandboxes de workflows lancent Claude Code, Codex ou OpenCode sur votre dépôt, avec les skills et serveurs MCP de la plateforme injectés automatiquement.

### AI Chat — ancré dans votre propre télémétrie

Posez une question sur votre projet et la réponse vient de vos données — traces, métriques, alarmes, déploiements et revenus — pas de la supposition d'un modèle générique. C'est **en lecture seule par défaut** : les actions d'écriture sont opt-in et, même activées, l'assistant propose la modification et attend votre confirmation.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/ai-chat-dark.png">
  <img alt="Le chat IA de Temps diagnostiquant un pic de latence au checkout à partir des traces, métriques et revenus du projet lui-même" src="assets/screenshots/ai-chat-light.png">
</picture>

### AI Gateway — un seul endpoint, vos propres clés

Apportez vos propres clés de fournisseur (OpenAI, Anthropic, xAI, Google Gemini) et appelez-les toutes via un unique endpoint compatible OpenAI — changez la base URL, gardez le SDK que vous utilisez déjà. Les clés restent chiffrées sur votre serveur, et chaque requête est attribuée : tokens, latence, taux d'erreur et coût estimé par modèle.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/ai-gateway-dark.png">
  <img alt="AI Gateway de Temps — clés de fournisseur personnelles (BYOK) derrière un endpoint compatible OpenAI" src="assets/screenshots/ai-gateway-light.png">
</picture>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/ai-usage-dark.png">
  <img alt="Analytique d'usage de l'AI Gateway de Temps — requêtes, tokens, latence, taux d'erreur et coût estimé" src="assets/screenshots/ai-usage-light.png">
</picture>

### Analytics web et session replay

Analytics web avec funnels, suivi des visiteurs et session replay (rrweb) intégrés — aucun service externe, aucune donnée ne quitte vos serveurs. C'est ce qu'aucune autre PaaS auto-hébergée ne propose.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/analytics-dark.png">
  <img alt="Analytics web Temps — visiteurs, sessions, pages, funnels" src="assets/screenshots/analytics-light.png">
</picture>

### Monitoring de disponibilité et alertes

Moniteurs d'uptime avec chronologie des statuts, plus des alertes en cas d'échec de déploiement, de crash à l'exécution, d'expiration de certificat ou de problème de sauvegarde. Soyez prévenu avant que les problèmes n'atteignent vos utilisateurs.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/uptime-dark.png">
  <img alt="Monitoring de disponibilité Temps — chronologie des statuts, pourcentage d'uptime, temps de réponse" src="assets/screenshots/uptime-light.png">
</picture>

### Suivi d'erreurs — compatible Sentry

Remplacement direct de Sentry : pointez le SDK Sentry officiel vers votre DSN Temps et obtenez des groupes d'erreurs, des stack traces avec contexte du code source et des alertes. Pas de tarification à l'événement.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/errors-dark.png">
  <img alt="Suivi d'erreurs Temps — groupes d'erreurs avec événements et chronologies" src="assets/screenshots/errors-light.png">
</picture>

### Logs de requêtes et visibilité du proxy

Chaque requête HTTP est journalisée avec méthode, chemin, statut, temps de réponse et métadonnées de routage — y compris le trafic détaillé par crawler IA (OpenAI, Anthropic, Perplexity, Google…). Fonctionne sur le moteur Pingora de Cloudflare avec TLS automatique via Let's Encrypt (HTTP-01 et DNS-01).

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/request-logs-dark.png">
  <img alt="Logs de requêtes du proxy Temps avec filtrage des agents IA" src="assets/screenshots/request-logs-light.png">
</picture>

### Emails transactionnels

Ajoutez des domaines d'envoi avec enregistrements DKIM depuis l'interface et envoyez via `@temps-sdk/node-sdk` — ou branchez AWS SES, Scaleway ou n'importe quel relais SMTP.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/email-dark.png">
  <img alt="Fournisseurs d'email Temps — SMTP, Scaleway et AWS SES" src="assets/screenshots/email-light.png">
</picture>

### OpenTelemetry — traces, métriques, logs et alertes

Pointez n'importe quel exporter OTLP vers Temps et vous obtenez les traces distribuées, les métriques et les logs structurés au même endroit que le reste. Les traces montrent la latence et les erreurs de chaque span entre services ; les métriques conservent vos golden signals ; les alertes se déclenchent à partir de ces métriques et arrivent dans une file unique où vous pouvez les acquitter ou les résoudre. Pas de Grafana, Prometheus, Jaeger ou Loki à faire tourner.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/traces-dark.png">
  <img alt="Traces distribuées Temps — latence par requête, nombre de spans et erreurs entre services" src="assets/screenshots/traces-light.png">
</picture>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/metrics-dark.png">
  <img alt="Métriques OpenTelemetry Temps — débit de requêtes, latence, signaux base de données et cache" src="assets/screenshots/metrics-light.png">
</picture>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/otel-logs-dark.png">
  <img alt="Logs structurés Temps — sévérité, service et message, corrélés aux traces" src="assets/screenshots/otel-logs-light.png">
</picture>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/alerts-dark.png">
  <img alt="Alertes Temps — alarmes actives, acquittées et résolues sur les métriques, conteneurs, disponibilité et bases de données" src="assets/screenshots/alerts-light.png">
</picture>

### Sandboxes IA — micro-VM Firecracker, auto-hébergés

**Une isolation matérielle réelle, pas seulement des conteneurs.** Les sandboxes tournent sur des **micro-VM Firecracker** — la technologie derrière AWS Lambda — avec **Docker** comme backend par défaut. Lancez `temps firecracker setup` et Temps route automatiquement les sandboxes vers des micro-VM ; chacune a son propre noyau, donc le code généré par un agent ne partage jamais le noyau de votre hôte.

**Un SDK drop-in.** `@temps-sdk/sandbox` est compatible avec la forme de `@vercel/sandbox` — changez de fournisseur en changeant l'import et l'URL de base :

```ts
import { Sandbox } from '@temps-sdk/sandbox'

const sandbox = await Sandbox.create({
  source: { type: 'git', url: 'https://github.com/example/repo.git', revision: 'main' },
})

const { stdout } = await sandbox.exec(['npm', 'test'])
const url = sandbox.domain(3000) // aperçu en direct d'un serveur de dev dans la VM
```

**Aperçus protégés par mot de passe.** Chaque port d'un sandbox peut être exposé sur une URL publique verrouillée par un mot de passe généré :

```bash
bunx @temps-sdk/cli sandbox password sbx_abc123 --rotate --length 32
bunx @temps-sdk/cli sandbox password sbx_abc123 --clear   # la rouvrir
```

Partagez une branche en cours d'exécution sans la publier au monde entier.

Également disponible via CLI et API REST. Exactement ce que vous paieriez sinon à E2B, Daytona ou Vercel Sandbox.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/sandboxes-dark.png">
  <img alt="Sandboxes Temps — sandboxes en cours d'exécution avec des extraits CLI, REST et SDK prêts à copier" src="assets/screenshots/sandboxes-light.png">
</picture>

Chaque sandbox dispose d'un shell, d'un modèle d'URL d'aperçu pour tout port qu'il ouvre, et d'une chronologie de tout ce qui lui est arrivé :

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/sandbox-detail-dark.png">
  <img alt="Détail d'un sandbox Temps — backend Docker/Firecracker, exécuteur de commandes dans le navigateur, modèle d'URL d'aperçu et aperçus protégés par mot de passe" src="assets/screenshots/sandbox-detail-light.png">
</picture>

### Tout dans un seul tableau de bord

Visiteurs, erreurs, statut des déploiements et état du monitoring par projet — un seul endroit au lieu de six onglets de navigateur.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/dashboard-dark.png">
  <img alt="Tableau de bord des projets Temps — tous les projets avec visiteurs et statut" src="assets/screenshots/dashboard-light.png">
</picture>

### Git push pour déployer et services managés

Poussez sur Git et Temps build, déploie et crée des URLs de préversion avec des rollouts sans interruption de service — n'importe quel langage, détecté automatiquement. Provisionnez Postgres, Redis, S3 (MinIO) et MongoDB aux côtés de vos applications ; la création, les sauvegardes et la suppression sont gérées pour vous.

### Compatible avec votre stack

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

<p align="center"><em>N'importe quel langage, n'importe quel framework. Détection automatique ou apportez votre propre Dockerfile.</em></p>

---

## Vous utilisez déjà autre chose ?

Temps importe votre configuration existante au lieu de vous demander de tout reconstruire. Pointez l'assistant vers votre plateforme actuelle, et il rapatrie tout — applications, bases de données *avec leurs données*, domaines et variables d'environnement.

**Plateformes auto-hébergées**

<p align="center">
<a href="https://coolify.io"><img src="https://img.shields.io/badge/Coolify-8B5CF6?logo=coolify&logoColor=fff&style=for-the-badge" alt="Coolify" /></a>
<a href="https://dokploy.com"><img src="https://img.shields.io/badge/Dokploy-0B0B0B?style=for-the-badge" alt="Dokploy" /></a>
<a href="https://caprover.com"><img src="https://img.shields.io/badge/CapRover-2196F3?logo=caprover&logoColor=fff&style=for-the-badge" alt="CapRover" /></a>
<a href="https://portainer.io"><img src="https://img.shields.io/badge/Portainer-13BEF9?logo=portainer&logoColor=fff&style=for-the-badge" alt="Portainer" /></a>
<a href="https://kamal-deploy.org"><img src="https://img.shields.io/badge/Kamal-1B1B1B?style=for-the-badge" alt="Kamal" /></a>
<a href="https://kubernetes.io"><img src="https://img.shields.io/badge/Kubernetes-326CE5?logo=kubernetes&logoColor=fff&style=for-the-badge" alt="Kubernetes" /></a>
<a href="https://docker.com"><img src="https://img.shields.io/badge/Docker-2496ED?logo=docker&logoColor=fff&style=for-the-badge" alt="Docker" /></a>
</p>

**Plateformes hébergées**

<p align="center">
<a href="https://vercel.com"><img src="https://img.shields.io/badge/Vercel-000?logo=vercel&logoColor=fff&style=for-the-badge" alt="Vercel" /></a>
<a href="https://netlify.com"><img src="https://img.shields.io/badge/Netlify-00C7B7?logo=netlify&logoColor=fff&style=for-the-badge" alt="Netlify" /></a>
<a href="https://railway.app"><img src="https://img.shields.io/badge/Railway-0B0D0E?logo=railway&logoColor=fff&style=for-the-badge" alt="Railway" /></a>
<a href="https://render.com"><img src="https://img.shields.io/badge/Render-000?logo=render&logoColor=fff&style=for-the-badge" alt="Render" /></a>
<a href="https://fly.io"><img src="https://img.shields.io/badge/Fly.io-24175B?logo=flydotio&logoColor=fff&style=for-the-badge" alt="Fly.io" /></a>
</p>

<p align="center"><em>L'import se lance depuis le tableau de bord — les icônes de plateforme se trouvent directement dans l'en-tête de votre page de projets.</em></p>

---

## Démarrage rapide

```bash
curl -fsSL https://temps.sh/deploy.sh | bash
```

**Testé sur :** Ubuntu 24.04 / 22.04 &nbsp;|&nbsp; Fonctionne aussi sur macOS

Vous préférez ne pas gérer de serveur ? [Temps Cloud](https://temps.sh/pricing?utm_source=github&utm_medium=repo&utm_content=cloud_cta_fr) fait tourner Temps pour vous sur une infrastructure managée.

---

## Ce que Temps remplace

| Ce que vous obtenez | Au lieu de payer |
|---|---|
| Déploiements Git + URLs de préversion | Vercel / Netlify / Railway (20 $+/mois) |
| Analytics web + funnels | PostHog / Plausible (0-450 $/mois) |
| Session replay | PostHog / FullStory (0-2000 $/mois) |
| Suivi d'erreurs | Sentry (26 $+/mois) |
| Traces, métriques et logs (OpenTelemetry) | Grafana Cloud / Datadog (0-500 $+/mois) |
| Monitoring de disponibilité | Better Uptime / Pingdom (20 $+/mois) |
| Postgres/Redis/S3 managés | AWS RDS / ElastiCache (50 $+/mois) |
| Emails transactionnels + DKIM | Resend / SendGrid (20-100 $/mois) |
| Sandboxes d'exécution de code IA | E2B / Daytona / Vercel Sandbox (150 $+/mois + usage) |
| AI gateway + suivi d'usage/coût | OpenRouter / Helicone / LangSmith (0-200 $+/mois) |
| Logs de requêtes + proxy | Cloudflare (0-200 $/mois) |
| **Total avec Temps** | **0 $ (auto-hébergé)** |

---

## Temps face aux alternatives

| Fonctionnalité | Temps | Coolify | Dokploy | Kamal | Railway | Render | Vercel |
|---|:---:|:---:|:---:|:---:|:---:|:---:|:---:|
| Auto-hébergé et open source | Oui | Oui | Oui | Oui | Non | Non | Non |
| Installation en un seul binaire | Oui | Non | Non | Outil CLI | -- | -- | -- |
| Déploiement par git push | Oui | Oui | Oui | Non | Oui | Oui | Oui |
| Déploiements de préversion | Oui | Oui | Oui | Non | Oui | Oui | Oui |
| TLS automatique (HTTP-01 + DNS-01) | Oui | Oui | Oui | Oui | Oui | Oui | Oui |
| Support de Docker Compose | Oui | Oui | Oui | Non | -- | -- | -- |
| Bibliothèque de templates en un clic | Non | 280+ | Oui | Non | Oui | Oui | Oui |
| Analytics web | Oui | Non | Non | Non | Non | Non | Option payante |
| Session replay | Oui | Non | Non | Non | Non | Non | Non |
| Suivi d'erreurs (compatible Sentry) | Oui | Non | Non | Non | Non | Non | Non |
| Traces + métriques + logs OpenTelemetry | Oui | Non | Non | Non | Non | Non | Traces (payant) |
| Monitoring de disponibilité | Oui | Non | Non | Non | Non | Non | Non |
| Emails transactionnels + DKIM | Oui | Non | Non | Non | Non | Non | Non |
| Sandboxes d'exécution de code (API) | Oui | Non | Non | Non | Non | Non | Sandbox (à l'usage) |
| AI gateway (BYOK) + assistant | Oui | Non | Non | Non | Non | Non | AI Gateway (payant) |
| Postgres / Redis managés | Oui | Oui | Oui | Non | Oui | Oui | Add-ons partenaires |
| Stockage compatible S3 | Oui | Non | Non | Non | Non | Non | Blob (payant) |
| Multi-nœud / clustering | Oui | Oui | Swarm | Oui | Managé | Managé | Managé |
| Fonctions edge / réseau edge mondial | Non | Non | Non | Non | Non | Non | Oui |
| Facturation par siège | Non | Non | Non | Non | 20 $/utilisateur (Pro) | Par utilisateur | 20 $/siège (Pro) |

**Là où les alternatives gagnent.** Coolify et Dokploy offrent des bibliothèques de templates en un clic (280+ applications sur Coolify) que Temps n'a pas encore, et tous deux ont des communautés bien plus grandes — Coolify à lui seul dépasse les 56k étoiles GitHub, tandis que Temps est le projet le plus récent de cette liste. Kamal est le choix le plus simple si tout ce que vous voulez, ce sont des déploiements Docker sans interruption pilotés depuis une CLI. Vercel et les autres plateformes managées vous offrent un réseau edge mondial, des fonctions edge et une absorption des attaques DDoS qu'un simple VPS ne peut pas égaler — et elles gèrent l'infrastructure à votre place, ce qui est une vraie valeur ajoutée si vous ne voulez jamais avoir à penser à un serveur.

Comparatifs détaillés et régulièrement mis à jour : [temps.sh/compare](https://temps.sh/compare?utm_source=github&utm_medium=repo&utm_content=compare_fr)

---

## Stack technique

- **Backend :** Rust, Axum, Sea-ORM, Pingora (le moteur de proxy de Cloudflare), Bollard (API Docker)
- **Frontend :** React 19, TypeScript, Tailwind CSS, shadcn/ui
- **Base de données :** PostgreSQL + TimescaleDB
- **Architecture :** 30+ crates en workspace, architecture de services en trois couches

---

## SDKs

| Package | Description |
|---|---|
| [`@temps-sdk/node-sdk`](https://www.npmjs.com/package/@temps-sdk/node-sdk) | Client de l'API plateforme + suivi d'erreurs compatible Sentry |
| [`@temps-sdk/react-analytics`](https://www.npmjs.com/package/@temps-sdk/react-analytics) | Analytics React, session replay, Web Vitals, suivi de l'engagement |
| [`@temps-sdk/kv`](https://www.npmjs.com/package/@temps-sdk/kv) | Store clé-valeur serverless |
| [`@temps-sdk/blob`](https://www.npmjs.com/package/@temps-sdk/blob) | Stockage de fichiers (compatible S3) |
| [`@temps-sdk/cli`](https://www.npmjs.com/package/@temps-sdk/cli) | Interface en ligne de commande |

<details>
<summary><strong>Exemples rapides</strong></summary>

**Analytics** -- enveloppez votre application React, tout le reste est automatique :

```tsx
import { TempsAnalyticsProvider } from '@temps-sdk/react-analytics';

export default function App({ children }) {
  return <TempsAnalyticsProvider>{children}</TempsAnalyticsProvider>;
}
```

**Suivi d'erreurs** -- compatible Sentry, remplacement direct :

```typescript
import { ErrorTracking } from '@temps-sdk/node-sdk';

ErrorTracking.init({ dsn: 'https://key@your-instance.temps.dev/1' });

try {
  riskyOperation();
} catch (error) {
  ErrorTracking.captureException(error);
}
```

**Store KV** -- API façon Redis, zéro configuration :

```typescript
import { kv } from '@temps-sdk/kv';

await kv.set('user:123', { name: 'Alice', plan: 'pro' }, { ex: 3600 });
const user = await kv.get('user:123');
```

**Stockage blob** -- uploadez et servez des fichiers :

```typescript
import { blob } from '@temps-sdk/blob';

const { url } = await blob.put('avatars/user-123.png', fileBuffer);
const files = await blob.list({ prefix: 'avatars/' });
```

</details>

---

## Communauté

- [GitHub Discussions](https://github.com/gotempsh/temps/discussions) — questions, idées et partage de projets
- [GitHub Issues](https://github.com/gotempsh/temps/issues) — rapports de bugs et demandes de fonctionnalités

Si Temps vous fait économiser un abonnement SaaS, [une étoile](https://github.com/gotempsh/temps) aide d'autres personnes à le découvrir.

---

## Historique des étoiles

<a href="https://www.star-history.com/#gotempsh/temps&Date">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/svg?repos=gotempsh/temps&type=Date&theme=dark" />
    <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/svg?repos=gotempsh/temps&type=Date" />
    <img alt="Graphique de l'historique des étoiles" src="https://api.star-history.com/svg?repos=gotempsh/temps&type=Date" />
  </picture>
</a>

---

## Contribuer

Les contributions sont les bienvenues. Consultez [CONTRIBUTING.md](CONTRIBUTING.md) pour les recommandations.

```bash
git clone https://github.com/gotempsh/temps.git
cd temps
cargo build --release
```

---

## Licence

Sous double licence [MIT](LICENSE-MIT) ou [Apache 2.0](LICENSE).

---

<div align="center">

[temps.sh](https://temps.sh/?utm_source=github&utm_medium=repo&utm_content=footer_fr) | [Documentation](https://temps.sh/docs?utm_source=github&utm_medium=repo&utm_content=footer_fr) | [GitHub](https://github.com/gotempsh/temps)

</div>
