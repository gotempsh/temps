<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="web/public/logo/temps-logo-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="web/public/logo/temps-logo-light.png">
  <img alt="Temps" src="web/public/logo/temps-logo-dark.png" width="280">
</picture>

**A alternativa open source ao Vercel + Sentry + PostHog + Pingdom + Resend + E2B.**
Deploys, analytics, session replay, error tracking, monitoramento de disponibilidade, e-mail transacional e sandboxes de IA -- em um único binário self-hosted.

[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE)
[![GitHub Release](https://img.shields.io/github/v/release/gotempsh/temps)](https://github.com/gotempsh/temps/releases)
[![Rust](https://img.shields.io/badge/rust-1.70%2B-orange.svg)](https://www.rust-lang.org/)
[![GitHub Stars](https://img.shields.io/github/stars/gotempsh/temps?style=social)](https://github.com/gotempsh/temps)

[Site](https://temps.sh) · [Documentação](https://temps.sh/docs) · [Guia Rápido](https://temps.sh/docs/introduction) · [Discussões](https://github.com/gotempsh/temps/discussions) · [Contributing](CONTRIBUTING.md)

[English](README.md) | [简体中文](README.zh-CN.md) | [Español](README.es.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [日本語](README.ja.md) | Português

</div>

---

```bash
curl -fsSL https://temps.sh/deploy.sh | bash
```

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/create-dark.png">
  <img alt="Importando um repositório público no Temps — os presets de framework são detectados automaticamente antes do deploy" src="assets/screenshots/create-light.png">
</picture>


Pare de pagar por 7 ferramentas SaaS diferentes. O Temps substitui sua plataforma de deploy, analytics, rastreamento de erros, session replay, monitoramento de uptime, e-mail transacional e sandboxes de execução de código para IA -- tudo self-hosted, tudo em um único binário.

---

## Funcionalidades

### Web Analytics e Session Replay

Web analytics com funis, rastreamento de visitantes e session replay (rrweb) integrados — sem serviços externos, sem dados saindo dos seus servidores. É isso que nenhum outro PaaS self-hosted tem.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/analytics-dark.png">
  <img alt="Web analytics do Temps — visitantes, sessões, páginas, funis" src="assets/screenshots/analytics-light.png">
</picture>

### Monitoramento de Uptime e Alertas

Monitores de uptime com linhas do tempo de status, além de alertas para falhas de deploy, crashes em runtime, expiração de certificados e saúde dos backups. Seja notificado antes que os problemas cheguem aos usuários.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/uptime-dark.png">
  <img alt="Monitoramento de uptime do Temps — linha do tempo de status, porcentagem de uptime, tempo de resposta" src="assets/screenshots/uptime-light.png">
</picture>

### Rastreamento de Erros — compatível com Sentry

Substituição drop-in do Sentry: aponte o SDK oficial do Sentry para o seu DSN do Temps e tenha grupos de erros, stack traces com contexto do código-fonte e alertas. Sem cobrança por evento.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/errors-dark.png">
  <img alt="Rastreamento de erros do Temps — grupos de erros com eventos e linhas do tempo" src="assets/screenshots/errors-light.png">
</picture>

### Logs de Requisições e Visibilidade do Proxy

Cada requisição HTTP registrada com método, caminho, status, tempo de resposta e metadados de roteamento — incluindo o tráfego por crawler de IA (OpenAI, Anthropic, Perplexity, Google…). Roda sobre o motor Pingora da Cloudflare com TLS automático via Let's Encrypt (HTTP-01 e DNS-01).

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/request-logs-dark.png">
  <img alt="Logs de requisições do proxy do Temps com filtragem por agentes de IA" src="assets/screenshots/request-logs-light.png">
</picture>

### E-mail Transacional

Adicione domínios de envio com registros DKIM pela interface e envie via `@temps-sdk/node-sdk` — ou conecte AWS SES, Scaleway ou qualquer relay SMTP.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/email-dark.png">
  <img alt="Provedores de e-mail do Temps — SMTP, Scaleway e AWS SES" src="assets/screenshots/email-light.png">
</picture>

### OpenTelemetry — traces, métricas, logs e alertas

Aponte qualquer exporter OTLP para o Temps e tenha traces distribuídos, métricas e logs estruturados no mesmo lugar que todo o resto. Os traces mostram latência e erros por span entre serviços; as métricas guardam seus golden signals; os alertas disparam a partir dessas métricas e caem em uma única fila para você reconhecer ou resolver. Sem Grafana, Prometheus, Jaeger ou Loki para manter.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/traces-dark.png">
  <img alt="Traces distribuídos do Temps — latência por requisição, contagem de spans e erros entre serviços" src="assets/screenshots/traces-light.png">
</picture>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/metrics-dark.png">
  <img alt="Métricas OpenTelemetry do Temps — taxa de requisições, latência, sinais de banco de dados e cache" src="assets/screenshots/metrics-light.png">
</picture>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/alerts-dark.png">
  <img alt="Alertas do Temps — alarmes disparando, reconhecidos e resolvidos de métricas, containers, disponibilidade e bancos de dados" src="assets/screenshots/alerts-light.png">
</picture>

### Sandboxes de IA — execução de código isolada

Suba sandboxes isolados para trabalho de agentes, testes e comandos pontuais via CLI, REST API ou SDK — uma API compatível com o Vercel Sandbox, com backends Docker ou microVM Firecracker. Exatamente o que você pagaria ao E2B ou ao Daytona.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/sandboxes-dark.png">
  <img alt="Sandboxes do Temps — crie sandboxes isolados via CLI, REST API ou SDK" src="assets/screenshots/sandboxes-light.png">
</picture>

### Tudo em um só Dashboard

Visitantes, erros, status de deploy e saúde do monitoramento por projeto — um único lugar em vez de seis abas do navegador.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/dashboard-dark.png">
  <img alt="Dashboard de projetos do Temps — todos os projetos com visitantes e status" src="assets/screenshots/dashboard-light.png">
</picture>

### Git Push para Deploy e Serviços Gerenciados

Faça push para o Git e o Temps compila, faz o deploy e cria URLs de preview com rollouts sem downtime — qualquer linguagem, detectada automaticamente. Provisione Postgres, Redis, S3 (MinIO) e MongoDB junto com suas aplicações; a criação, os backups e o desprovisionamento ficam por conta do Temps.

### Funciona com a sua stack

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

<p align="center"><em>Qualquer linguagem, qualquer framework. Detecção automática ou traga seu próprio Dockerfile.</em></p>

---

## Guia Rápido

```bash
curl -fsSL https://temps.sh/deploy.sh | bash
```

**Testado em:** Ubuntu 24.04 / 22.04 &nbsp;|&nbsp; Também funciona no macOS

Prefere não gerenciar um servidor? O [Temps Cloud](https://temps.sh/pricing) roda o Temps para você em infraestrutura gerenciada.

---

## O que o Temps substitui

| O que você ganha | Em vez de pagar por |
|---|---|
| Deploys via Git + URLs de preview | Vercel / Netlify / Railway (US$ 20+/mês) |
| Web analytics + funis | PostHog / Plausible (US$ 0-450/mês) |
| Session replay | PostHog / FullStory (US$ 0-2000/mês) |
| Rastreamento de erros | Sentry (US$ 26+/mês) |
| Traces, métricas e logs (OpenTelemetry) | Grafana Cloud / Datadog ($0-500+/mês) |
| Monitoramento de uptime | Better Uptime / Pingdom (US$ 20+/mês) |
| Postgres/Redis/S3 gerenciados | AWS RDS / ElastiCache (US$ 50+/mês) |
| E-mail transacional + DKIM | Resend / SendGrid (US$ 20-100/mês) |
| Sandboxes de execução de código para IA | E2B / Daytona / Vercel Sandbox ($150+/mês + uso) |
| Logs de requisições + proxy | Cloudflare (US$ 0-200/mês) |
| **Total com o Temps** | **US$ 0 (self-hosted)** |

---

## Temps vs. Alternativas

| Funcionalidade | Temps | Coolify | Dokploy | Kamal | Railway | Render | Vercel |
|---|:---:|:---:|:---:|:---:|:---:|:---:|:---:|
| Self-hosted e open source | Sim | Sim | Sim | Sim | Não | Não | Não |
| Instalação em binário único | Sim | Não | Não | Ferramenta CLI | -- | -- | -- |
| Deploy via git push | Sim | Sim | Sim | Não | Sim | Sim | Sim |
| Deploys de preview | Sim | Sim | Sim | Não | Sim | Sim | Sim |
| TLS automático (HTTP-01 + DNS-01) | Sim | Sim | Sim | Sim | Sim | Sim | Sim |
| Suporte a Docker Compose | Sim | Sim | Sim | Não | -- | -- | -- |
| Biblioteca de templates one-click | Não | 280+ | Sim | Não | Sim | Sim | Sim |
| Web analytics | Sim | Não | Não | Não | Não | Não | Add-on pago |
| Session replay | Sim | Não | Não | Não | Não | Não | Não |
| Rastreamento de erros (compatível com Sentry) | Sim | Não | Não | Não | Não | Não | Não |
| Traces + métricas + logs OpenTelemetry | Sim | Não | Não | Não | Não | Não | Traces (pago) |
| Monitoramento de uptime | Sim | Não | Não | Não | Não | Não | Não |
| E-mail transacional + DKIM | Sim | Não | Não | Não | Não | Não | Não |
| Sandboxes de execução de código (API) | Sim | Não | Não | Não | Não | Não | Sandbox (por uso) |
| Postgres / Redis gerenciados | Sim | Sim | Sim | Não | Sim | Sim | Add-ons de parceiros |
| Armazenamento compatível com S3 | Sim | Não | Não | Não | Não | Não | Blob (pago) |
| Multi-node / clustering | Sim | Sim | Swarm | Sim | Gerenciado | Gerenciado | Gerenciado |
| Edge functions / rede edge global | Não | Não | Não | Não | Não | Não | Sim |
| Cobrança por usuário | Não | Não | Não | Não | US$ 20/usuário (Pro) | Por usuário | US$ 20/assento (Pro) |

**Onde as alternativas ganham.** Coolify e Dokploy têm bibliotecas de templates one-click (280+ apps no Coolify) que o Temps ainda não tem, e ambos contam com comunidades muito maiores — só o Coolify tem mais de 56 mil estrelas no GitHub, enquanto o Temps é o projeto mais novo desta lista. O Kamal é a escolha mais simples se tudo o que você quer são deploys Docker sem downtime comandados pela CLI. A Vercel e as demais plataformas gerenciadas oferecem uma rede edge global, edge functions e absorção de DDoS que um único VPS não consegue igualar — e elas operam a infraestrutura por você, o que é um valor real se você nunca quiser se preocupar com um servidor.

Comparações detalhadas e atualizadas regularmente: [temps.sh/compare](https://temps.sh/compare)

---

## Stack Tecnológica

- **Backend:** Rust, Axum, Sea-ORM, Pingora (motor de proxy da Cloudflare), Bollard (API do Docker)
- **Frontend:** React 19, TypeScript, Tailwind CSS, shadcn/ui
- **Banco de dados:** PostgreSQL + TimescaleDB
- **Arquitetura:** 30+ crates no workspace, arquitetura de serviços em três camadas

---

## SDKs

| Pacote | Descrição |
|---|---|
| [`@temps-sdk/node-sdk`](https://www.npmjs.com/package/@temps-sdk/node-sdk) | Cliente da API da plataforma + rastreamento de erros compatível com Sentry |
| [`@temps-sdk/react-analytics`](https://www.npmjs.com/package/@temps-sdk/react-analytics) | Analytics para React, session replay, Web Vitals, rastreamento de engajamento |
| [`@temps-sdk/kv`](https://www.npmjs.com/package/@temps-sdk/kv) | Armazenamento chave-valor serverless |
| [`@temps-sdk/blob`](https://www.npmjs.com/package/@temps-sdk/blob) | Armazenamento de arquivos (compatível com S3) |
| [`@temps-sdk/cli`](https://www.npmjs.com/package/@temps-sdk/cli) | Interface de linha de comando |

<details>
<summary><strong>Exemplos rápidos</strong></summary>

**Analytics** -- envolva sua aplicação React e o resto é automático:

```tsx
import { TempsAnalyticsProvider } from '@temps-sdk/react-analytics';

export default function App({ children }) {
  return <TempsAnalyticsProvider>{children}</TempsAnalyticsProvider>;
}
```

**Rastreamento de erros** -- compatível com Sentry, substituição drop-in:

```typescript
import { ErrorTracking } from '@temps-sdk/node-sdk';

ErrorTracking.init({ dsn: 'https://key@your-instance.temps.dev/1' });

try {
  riskyOperation();
} catch (error) {
  ErrorTracking.captureException(error);
}
```

**KV store** -- API estilo Redis, zero configuração:

```typescript
import { kv } from '@temps-sdk/kv';

await kv.set('user:123', { name: 'Alice', plan: 'pro' }, { ex: 3600 });
const user = await kv.get('user:123');
```

**Armazenamento de blobs** -- faça upload e sirva arquivos:

```typescript
import { blob } from '@temps-sdk/blob';

const { url } = await blob.put('avatars/user-123.png', fileBuffer);
const files = await blob.list({ prefix: 'avatars/' });
```

</details>

---

## Comunidade

- [GitHub Discussions](https://github.com/gotempsh/temps/discussions) — perguntas, ideias e mostre seu projeto
- [GitHub Issues](https://github.com/gotempsh/temps/issues) — relatos de bugs e pedidos de funcionalidades

Se o Temps te livrar de uma fatura de SaaS, [uma estrela](https://github.com/gotempsh/temps) ajuda outras pessoas a encontrá-lo.

---

## Histórico de Estrelas

<a href="https://www.star-history.com/#gotempsh/temps&Date">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/svg?repos=gotempsh/temps&type=Date&theme=dark" />
    <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/svg?repos=gotempsh/temps&type=Date" />
    <img alt="Gráfico do Histórico de Estrelas" src="https://api.star-history.com/svg?repos=gotempsh/temps&type=Date" />
  </picture>
</a>

---

## Contribuindo

Contribuições são bem-vindas. Consulte o [CONTRIBUTING.md](CONTRIBUTING.md) para as diretrizes.

```bash
git clone https://github.com/gotempsh/temps.git
cd temps
cargo build --release
```

---

## Licença

Licenciado sob dupla licença: [MIT](LICENSE-MIT) ou [Apache 2.0](LICENSE).

---

<div align="center">

[temps.sh](https://temps.sh) | [Documentação](https://temps.sh/docs) | [GitHub](https://github.com/gotempsh/temps)

</div>
