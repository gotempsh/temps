<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="web/public/logo/temps-logo-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="web/public/logo/temps-logo-light.png">
  <img alt="Temps" src="web/public/logo/temps-logo-dark.png" width="280">
</picture>

**A alternativa open source ao Vercel + Sentry + PostHog + Pingdom + Resend + E2B.**
Deploys, analytics, session replay, error tracking, monitoramento de disponibilidade, e-mail transacional e sandboxes de IA -- em um único binário self-hosted.

**Nativo para IA:** mais de 440 operações de CLI e skills prontas para Claude Code, Codex e OpenCode.

[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE)
[![GitHub Release](https://img.shields.io/github/v/release/gotempsh/temps)](https://github.com/gotempsh/temps/releases)
[![Rust](https://img.shields.io/badge/rust-1.70%2B-orange.svg)](https://www.rust-lang.org/)
[![GitHub Stars](https://img.shields.io/github/stars/gotempsh/temps?style=social)](https://github.com/gotempsh/temps)

[Site](https://temps.sh/?utm_source=github&utm_medium=repo&utm_content=header_pt-BR) · [Documentação](https://temps.sh/docs?utm_source=github&utm_medium=repo&utm_content=header_pt-BR) · [Guia Rápido](https://temps.sh/docs/introduction?utm_source=github&utm_medium=repo&utm_content=header_pt-BR) · [Discussões](https://github.com/gotempsh/temps/discussions) · [Contributing](CONTRIBUTING.md)

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

### Nativo para IA — mais de 440 operações que um agente pode executar

Toda operação do dashboard também é um comando de CLI — **mais de 440 em 69 grupos** — e o Temps já traz as [skills](skills/) que ensinam um agente a usá-los. Coloque-as no **Claude Code**, **Codex**, **OpenCode** ou em qualquer harness que leia `.claude/skills/`, e seu agente consegue fazer deploy, inspecionar traces, rodar migrações ou adicionar um domínio sem você escrever a cola.

```bash
bunx @temps-sdk/cli projects list
bunx @temps-sdk/cli deploy my-app --environment production
bunx @temps-sdk/cli analytics ai-agents -p my-app --period 7d
```

O Temps também roda esses agentes para você: os sandboxes de workflow executam Claude Code, Codex ou OpenCode no seu repositório, com as skills e servidores MCP da plataforma injetados automaticamente.

### AI Chat — ancorado na sua própria telemetria

Pergunte sobre o seu projeto e a resposta vem dos seus dados — traces, métricas, alarmes, deploys e receita — não do palpite de um modelo genérico. É **somente leitura por padrão**: ações de escrita são opt-in e, mesmo assim, o assistente propõe a mudança e espera a sua confirmação.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/ai-chat-dark.png">
  <img alt="O chat de IA do Temps diagnosticando um pico de latência no checkout a partir dos traces, métricas e dados de receita do próprio projeto" src="assets/screenshots/ai-chat-light.png">
</picture>

### AI Gateway — um endpoint, suas próprias chaves

Traga suas próprias chaves de provedor (OpenAI, Anthropic, xAI, Google Gemini) e chame todas por um único endpoint compatível com OpenAI — troque a base URL e mantenha o SDK que você já usa. As chaves ficam criptografadas no seu servidor, e cada requisição é atribuída: tokens, latência, taxa de erro e custo estimado por modelo.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/ai-gateway-dark.png">
  <img alt="AI Gateway do Temps — chaves de provedor próprias (BYOK) atrás de um endpoint compatível com OpenAI" src="assets/screenshots/ai-gateway-light.png">
</picture>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/ai-usage-dark.png">
  <img alt="Análise de uso do AI Gateway do Temps — requisições, tokens, latência, taxa de erro e custo estimado" src="assets/screenshots/ai-usage-light.png">
</picture>

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
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/otel-logs-dark.png">
  <img alt="Logs estruturados do Temps — severidade, serviço e mensagem, correlacionados com os traces" src="assets/screenshots/otel-logs-light.png">
</picture>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/alerts-dark.png">
  <img alt="Alertas do Temps — alarmes disparando, reconhecidos e resolvidos de métricas, containers, disponibilidade e bancos de dados" src="assets/screenshots/alerts-light.png">
</picture>

### Sandboxes de IA — microVMs Firecracker, auto-hospedados

**Isolamento real em nível de hardware, não apenas contêineres.** Os sandboxes rodam em **microVMs Firecracker** — a mesma tecnologia por trás do AWS Lambda — com **Docker** como backend padrão. Rode `temps firecracker setup` e o Temps roteia os sandboxes para microVMs automaticamente; cada um com seu próprio kernel, então código gerado por agentes nunca compartilha kernel com o seu host.

**Um SDK drop-in.** `@temps-sdk/sandbox` é compatível com o formato do `@vercel/sandbox` — troque de provedor mudando o import e a URL base:

```ts
import { Sandbox } from '@temps-sdk/sandbox'

const sandbox = await Sandbox.create({
  source: { type: 'git', url: 'https://github.com/example/repo.git', revision: 'main' },
})

const { stdout } = await sandbox.exec(['npm', 'test'])
const url = sandbox.domain(3000) // preview ao vivo de um dev server dentro da VM
```

**Previews protegidos por senha.** Qualquer porta do sandbox pode ser exposta em uma URL pública protegida por uma senha gerada:

```bash
bunx @temps-sdk/cli sandbox password sbx_abc123 --rotate --length 32
bunx @temps-sdk/cli sandbox password sbx_abc123 --clear   # abre de novo
```

Compartilhe uma branch em execução sem publicá-la para o mundo.

Também disponível via CLI e REST API. Exatamente o que você pagaria ao E2B, Daytona ou Vercel Sandbox.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/sandboxes-dark.png">
  <img alt="Sandboxes do Temps — sandboxes em execução com trechos de CLI, REST e SDK prontos para copiar" src="assets/screenshots/sandboxes-light.png">
</picture>

Cada sandbox ganha um shell, um modelo de URL de preview para qualquer porta que abrir, e uma linha do tempo de tudo o que aconteceu com ele:

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/sandbox-detail-dark.png">
  <img alt="Detalhe de sandbox no Temps — backend Docker/Firecracker, executor de comandos no navegador, modelo de URL de preview e previews protegidos por senha" src="assets/screenshots/sandbox-detail-light.png">
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

## Já está rodando em outro lugar?

O Temps importa sua configuração existente em vez de pedir que você reconstrua tudo. Aponte o assistente para sua plataforma atual e ele traz tudo junto — apps, bancos de dados *com os dados*, domínios e variáveis de ambiente.

**Plataformas autogerenciadas**

<p align="center">
<a href="https://coolify.io"><img src="https://img.shields.io/badge/Coolify-8B5CF6?logo=coolify&logoColor=fff&style=for-the-badge" alt="Coolify" /></a>
<a href="https://dokploy.com"><img src="https://img.shields.io/badge/Dokploy-0B0B0B?style=for-the-badge" alt="Dokploy" /></a>
<a href="https://caprover.com"><img src="https://img.shields.io/badge/CapRover-2196F3?logo=caprover&logoColor=fff&style=for-the-badge" alt="CapRover" /></a>
<a href="https://portainer.io"><img src="https://img.shields.io/badge/Portainer-13BEF9?logo=portainer&logoColor=fff&style=for-the-badge" alt="Portainer" /></a>
<a href="https://kamal-deploy.org"><img src="https://img.shields.io/badge/Kamal-1B1B1B?style=for-the-badge" alt="Kamal" /></a>
<a href="https://kubernetes.io"><img src="https://img.shields.io/badge/Kubernetes-326CE5?logo=kubernetes&logoColor=fff&style=for-the-badge" alt="Kubernetes" /></a>
<a href="https://docker.com"><img src="https://img.shields.io/badge/Docker-2496ED?logo=docker&logoColor=fff&style=for-the-badge" alt="Docker" /></a>
</p>

**Plataformas hospedadas**

<p align="center">
<a href="https://vercel.com"><img src="https://img.shields.io/badge/Vercel-000?logo=vercel&logoColor=fff&style=for-the-badge" alt="Vercel" /></a>
<a href="https://netlify.com"><img src="https://img.shields.io/badge/Netlify-00C7B7?logo=netlify&logoColor=fff&style=for-the-badge" alt="Netlify" /></a>
<a href="https://railway.app"><img src="https://img.shields.io/badge/Railway-0B0D0E?logo=railway&logoColor=fff&style=for-the-badge" alt="Railway" /></a>
<a href="https://render.com"><img src="https://img.shields.io/badge/Render-000?logo=render&logoColor=fff&style=for-the-badge" alt="Render" /></a>
<a href="https://fly.io"><img src="https://img.shields.io/badge/Fly.io-24175B?logo=flydotio&logoColor=fff&style=for-the-badge" alt="Fly.io" /></a>
</p>

<p align="center"><em>A importação é feita pelo painel — os ícones das plataformas ficam no cabeçalho da sua página de projetos.</em></p>

---

## Guia Rápido

```bash
curl -fsSL https://temps.sh/deploy.sh | bash
```

**Testado em:** Ubuntu 24.04 / 22.04 &nbsp;|&nbsp; Também funciona no macOS

Prefere não gerenciar um servidor? O [Temps Cloud](https://temps.sh/pricing?utm_source=github&utm_medium=repo&utm_content=cloud_cta_pt-BR) roda o Temps para você em infraestrutura gerenciada.

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
| AI gateway + rastreamento de uso/custo | OpenRouter / Helicone / LangSmith ($0-200+/mês) |
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
| AI gateway (BYOK) + assistente | Sim | Não | Não | Não | Não | Não | AI Gateway (pago) |
| Postgres / Redis gerenciados | Sim | Sim | Sim | Não | Sim | Sim | Add-ons de parceiros |
| Armazenamento compatível com S3 | Sim | Não | Não | Não | Não | Não | Blob (pago) |
| Multi-node / clustering | Sim | Sim | Swarm | Sim | Gerenciado | Gerenciado | Gerenciado |
| Edge functions / rede edge global | Não | Não | Não | Não | Não | Não | Sim |
| Cobrança por usuário | Não | Não | Não | Não | US$ 20/usuário (Pro) | Por usuário | US$ 20/assento (Pro) |

**Onde as alternativas ganham.** Coolify e Dokploy têm bibliotecas de templates one-click (280+ apps no Coolify) que o Temps ainda não tem, e ambos contam com comunidades muito maiores — só o Coolify tem mais de 56 mil estrelas no GitHub, enquanto o Temps é o projeto mais novo desta lista. O Kamal é a escolha mais simples se tudo o que você quer são deploys Docker sem downtime comandados pela CLI. A Vercel e as demais plataformas gerenciadas oferecem uma rede edge global, edge functions e absorção de DDoS que um único VPS não consegue igualar — e elas operam a infraestrutura por você, o que é um valor real se você nunca quiser se preocupar com um servidor.

Comparações detalhadas e atualizadas regularmente: [temps.sh/compare](https://temps.sh/compare?utm_source=github&utm_medium=repo&utm_content=compare_pt-BR)

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

<a href="https://star-history.dera.page/#gotempsh/temps&Date">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://star-history.dera.page/svg?repos=gotempsh/temps&type=Date&theme=dark" />
    <source media="(prefers-color-scheme: light)" srcset="https://star-history.dera.page/svg?repos=gotempsh/temps&type=Date" />
    <img alt="Gráfico do Histórico de Estrelas" src="https://star-history.dera.page/svg?repos=gotempsh/temps&type=Date" />
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

[temps.sh](https://temps.sh/?utm_source=github&utm_medium=repo&utm_content=footer_pt-BR) | [Documentação](https://temps.sh/docs?utm_source=github&utm_medium=repo&utm_content=footer_pt-BR) | [GitHub](https://github.com/gotempsh/temps)

</div>
