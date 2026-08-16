<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="web/public/logo/temps-logo-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="web/public/logo/temps-logo-light.png">
  <img alt="Temps" src="web/public/logo/temps-logo-dark.png" width="280">
</picture>

**Vercel + Sentry + PostHog + Pingdom + Resend + E2B 的开源替代品。**
部署、分析、会话回放、错误追踪、可用性监控、事务性邮件与 AI 沙箱 —— 一个自托管的二进制文件搞定。

**AI 原生：** 440+ 条 CLI 操作，以及可直接放入 Claude Code、Codex 与 OpenCode 的技能。

[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE)
[![GitHub Release](https://img.shields.io/github/v/release/gotempsh/temps)](https://github.com/gotempsh/temps/releases)
[![Rust](https://img.shields.io/badge/rust-1.70%2B-orange.svg)](https://www.rust-lang.org/)
[![GitHub Stars](https://img.shields.io/github/stars/gotempsh/temps?style=social)](https://github.com/gotempsh/temps)

[官网](https://temps.sh/?utm_source=github&utm_medium=repo&utm_content=header_zh) · [文档](https://temps.sh/docs?utm_source=github&utm_medium=repo&utm_content=header_zh) · [快速开始](https://temps.sh/docs/introduction?utm_source=github&utm_medium=repo&utm_content=header_zh) · [讨论区](https://github.com/gotempsh/temps/discussions) · [Contributing](CONTRIBUTING.md)

[English](README.md) | 简体中文 | [Español](README.es.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [日本語](README.ja.md) | [Português](README.pt-BR.md)

</div>

---

```bash
curl -fsSL https://temps.sh/deploy.sh | bash
```

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/create-dark.png">
  <img alt="在 Temps 中导入公开仓库 —— 部署前自动检测框架预设" src="assets/screenshots/create-light.png">
</picture>


别再为 7 个不同的 SaaS 工具付费了。Temps 一次性替代你的部署平台、网站分析、错误追踪、会话回放、可用性监控、事务性邮件和 AI 代码执行沙箱 —— 全部自托管，全部集成在一个二进制文件里。

---

## 功能特性

### AI 原生 —— 440+ 项可供智能体驱动的操作

控制台里的每一个操作都有对应的 CLI 命令 —— **69 个命令组、440+ 条命令** —— 而且 Temps 直接提供了教智能体使用它们的 [skills](skills/)。把它们放进 **Claude Code**、**Codex**、**OpenCode** 或任何读取 `.claude/skills/` 的环境，你的智能体就能部署应用、查看链路追踪、执行迁移或添加域名，你不用再写任何胶水代码。

```bash
bunx @temps-sdk/cli projects list
bunx @temps-sdk/cli deploy my-app --environment production
bunx @temps-sdk/cli analytics ai-agents -p my-app --period 7d
```

Temps 也能替你运行这些智能体：工作流沙箱会针对你的仓库执行 Claude Code、Codex 或 OpenCode，并自动注入平台级的技能与 MCP 服务器。

### AI 对话 —— 基于你自己的可观测数据

询问你的项目，答案来自你自己的数据 —— 链路追踪、指标、告警、部署和收入 —— 而不是通用模型的猜测。默认**只读**：写操作需要显式开启，且即使开启后，助手也会先提出变更方案并等你确认。

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/ai-chat-dark.png">
  <img alt="Temps AI 对话正在依据项目自身的链路追踪、指标与收入数据诊断结账延迟骤增" src="assets/screenshots/ai-chat-light.png">
</picture>

### AI 网关 —— 一个端点，用你自己的密钥

自带各家服务商的密钥（OpenAI、Anthropic、xAI、Google Gemini），全部通过一个兼容 OpenAI 的端点调用 —— 只需替换 base URL，继续用你现在的 SDK。密钥加密存放在你自己的服务器上，每个请求都有归因：token 数、延迟、错误率和按模型估算的成本。

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/ai-gateway-dark.png">
  <img alt="Temps AI 网关 —— 自带密钥（BYOK）的服务商统一在一个兼容 OpenAI 的端点后面" src="assets/screenshots/ai-gateway-light.png">
</picture>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/ai-usage-dark.png">
  <img alt="Temps AI 网关用量分析 —— 请求数、token、延迟、错误率与估算成本" src="assets/screenshots/ai-usage-light.png">
</picture>

### 网站分析与会话回放

内置带漏斗分析、访客追踪和会话回放（rrweb）的网站分析 —— 无需外部服务，数据不会离开你的服务器。这是其他自托管 PaaS 都没有的能力。

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/analytics-dark.png">
  <img alt="Temps 网站分析 —— 访客、会话、页面、漏斗" src="assets/screenshots/analytics-light.png">
</picture>

### 可用性监控与告警

带状态时间线的可用性监控，以及针对部署失败、运行时崩溃、证书过期和备份健康状况的告警。在问题波及用户之前就收到通知。

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/uptime-dark.png">
  <img alt="Temps 可用性监控 —— 状态时间线、可用率、响应时间" src="assets/screenshots/uptime-light.png">
</picture>

### 错误追踪 —— 兼容 Sentry

可直接替换 Sentry：将官方 Sentry SDK 指向你的 Temps DSN，即可获得错误分组、带源码上下文的堆栈跟踪和告警。没有按事件计费。

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/errors-dark.png">
  <img alt="Temps 错误追踪 —— 带事件和时间线的错误分组" src="assets/screenshots/errors-light.png">
</picture>

### 请求日志与代理可观测性

每个 HTTP 请求都会记录方法、路径、状态码、响应时间和路由元数据 —— 还包括各家 AI 爬虫的流量明细（OpenAI、Anthropic、Perplexity、Google……）。运行在 Cloudflare 的 Pingora 引擎之上，通过 Let's Encrypt 自动签发 TLS 证书（HTTP-01 和 DNS-01）。

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/request-logs-dark.png">
  <img alt="Temps 代理请求日志，支持按 AI 爬虫过滤" src="assets/screenshots/request-logs-light.png">
</picture>

### 事务性邮件

通过 UI 添加带 DKIM 记录的发件域名，并通过 `@temps-sdk/node-sdk` 发送邮件 —— 也可以接入 AWS SES、Scaleway 或任意 SMTP 中继。

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/email-dark.png">
  <img alt="Temps 邮件服务商 —— SMTP、Scaleway 和 AWS SES" src="assets/screenshots/email-light.png">
</picture>

### OpenTelemetry —— 链路追踪、指标、日志与告警

把任意 OTLP exporter 指向 Temps，分布式链路追踪、指标和结构化日志就会和其他数据汇聚在同一个地方。链路追踪展示跨服务每个 span 的耗时与错误；指标持续记录你的黄金信号；告警基于这些指标触发，并集中到一个队列中供你确认或解决。无需再运维 Grafana、Prometheus、Jaeger 或 Loki。

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/traces-dark.png">
  <img alt="Temps 分布式链路追踪 —— 每个请求的耗时、span 数量与跨服务错误" src="assets/screenshots/traces-light.png">
</picture>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/metrics-dark.png">
  <img alt="Temps OpenTelemetry 指标 —— 请求速率、延迟、数据库与缓存信号" src="assets/screenshots/metrics-light.png">
</picture>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/otel-logs-dark.png">
  <img alt="Temps 结构化日志 —— 严重级别、服务与消息，并与链路追踪关联" src="assets/screenshots/otel-logs-light.png">
</picture>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/alerts-dark.png">
  <img alt="Temps 告警 —— 覆盖指标、容器、可用性与数据库的触发中、已确认与已解决告警" src="assets/screenshots/alerts-light.png">
</picture>

### AI 沙箱 —— Firecracker 微虚拟机，自托管

**真正的硬件级隔离，不只是容器。** 沙箱运行在 **Firecracker 微虚拟机**上 —— 与 AWS Lambda 背后是同一套技术 —— 默认后端为 **Docker**。运行 `temps firecracker setup`，Temps 会自动把沙箱调度到微虚拟机；每个沙箱都有独立内核，智能体生成的不可信代码绝不会与你的宿主机共享内核。

**可直接替换的 SDK。** `@temps-sdk/sandbox` 兼容 `@vercel/sandbox` 的形态 —— 换个 import 和 base URL 就能切换服务商：

```ts
import { Sandbox } from '@temps-sdk/sandbox'

const sandbox = await Sandbox.create({
  source: { type: 'git', url: 'https://github.com/example/repo.git', revision: 'main' },
})

const { stdout } = await sandbox.exec(['npm', 'test'])
const url = sandbox.domain(3000) // 虚拟机内开发服务器的实时预览
```

**带密码保护的预览。** 沙箱的任意端口都可以映射到公开预览 URL，并用生成的密码锁住：

```bash
bunx @temps-sdk/cli sandbox password sbx_abc123 --rotate --length 32
bunx @temps-sdk/cli sandbox password sbx_abc123 --clear   # 重新开放
```

分享一个正在运行的分支，而不必把它公开给所有人。

同样支持 CLI 和 REST API。这正是你原本要为 E2B、Daytona 或 Vercel Sandbox 付费的能力。

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/sandboxes-dark.png">
  <img alt="Temps 沙箱 —— 运行中的沙箱，附带可直接复制的 CLI、REST 和 SDK 代码片段" src="assets/screenshots/sandboxes-light.png">
</picture>

每个沙箱都自带 shell、任意绑定端口的预览 URL 模板，以及一条完整的操作时间线：

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/sandbox-detail-dark.png">
  <img alt="Temps 沙箱详情 —— Docker/Firecracker 后端、浏览器内命令执行器、预览 URL 模板与密码保护的预览" src="assets/screenshots/sandbox-detail-light.png">
</picture>

### 一个仪表盘掌握全局

每个项目的访客、错误、部署状态和监控健康状况 —— 一个页面搞定，不用再开六个浏览器标签页。

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/dashboard-dark.png">
  <img alt="Temps 项目仪表盘 —— 所有项目的访客与状态" src="assets/screenshots/dashboard-light.png">
</picture>

### Git Push 即部署与托管服务

推送到 Git，Temps 自动构建、部署并生成预览 URL，零停机发布 —— 支持任何语言，自动检测。在应用旁边一键开通 Postgres、Redis、S3（MinIO）和 MongoDB；创建、备份和销毁都由 Temps 负责。

### 兼容你的技术栈

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

<p align="center"><em>任何语言、任何框架。自动检测，或自带 Dockerfile。</em></p>

---

## 已经在其他平台上运行了?

Temps 会直接导入你现有的配置,而不是要求你重新搭建。只需在向导中选择你当前使用的平台,应用、*带数据的*数据库、域名和环境变量都会一并迁移过来。

**自托管平台**

<p align="center">
<a href="https://coolify.io"><img src="https://img.shields.io/badge/Coolify-8B5CF6?logo=coolify&logoColor=fff&style=for-the-badge" alt="Coolify" /></a>
<a href="https://dokploy.com"><img src="https://img.shields.io/badge/Dokploy-0B0B0B?style=for-the-badge" alt="Dokploy" /></a>
<a href="https://caprover.com"><img src="https://img.shields.io/badge/CapRover-2196F3?logo=caprover&logoColor=fff&style=for-the-badge" alt="CapRover" /></a>
<a href="https://portainer.io"><img src="https://img.shields.io/badge/Portainer-13BEF9?logo=portainer&logoColor=fff&style=for-the-badge" alt="Portainer" /></a>
<a href="https://kamal-deploy.org"><img src="https://img.shields.io/badge/Kamal-1B1B1B?style=for-the-badge" alt="Kamal" /></a>
<a href="https://kubernetes.io"><img src="https://img.shields.io/badge/Kubernetes-326CE5?logo=kubernetes&logoColor=fff&style=for-the-badge" alt="Kubernetes" /></a>
<a href="https://docker.com"><img src="https://img.shields.io/badge/Docker-2496ED?logo=docker&logoColor=fff&style=for-the-badge" alt="Docker" /></a>
</p>

**托管平台**

<p align="center">
<a href="https://vercel.com"><img src="https://img.shields.io/badge/Vercel-000?logo=vercel&logoColor=fff&style=for-the-badge" alt="Vercel" /></a>
<a href="https://netlify.com"><img src="https://img.shields.io/badge/Netlify-00C7B7?logo=netlify&logoColor=fff&style=for-the-badge" alt="Netlify" /></a>
<a href="https://railway.app"><img src="https://img.shields.io/badge/Railway-0B0D0E?logo=railway&logoColor=fff&style=for-the-badge" alt="Railway" /></a>
<a href="https://render.com"><img src="https://img.shields.io/badge/Render-000?logo=render&logoColor=fff&style=for-the-badge" alt="Render" /></a>
<a href="https://fly.io"><img src="https://img.shields.io/badge/Fly.io-24175B?logo=flydotio&logoColor=fff&style=for-the-badge" alt="Fly.io" /></a>
</p>

<p align="center"><em>导入操作在控制台中完成 — 平台图标就在项目页面的顶部。</em></p>

---

## 快速开始

```bash
curl -fsSL https://temps.sh/deploy.sh | bash
```

**已测试系统：** Ubuntu 24.04 / 22.04 &nbsp;|&nbsp; macOS 也可运行

不想自己维护服务器？[Temps Cloud](https://temps.sh/pricing?utm_source=github&utm_medium=repo&utm_content=cloud_cta_zh) 可以在托管基础设施上为你运行 Temps。

---

## Temps 能替代什么

| 你得到的 | 不用再付费的 |
|---|---|
| Git 部署 + 预览 URL | Vercel / Netlify / Railway（$20+/月） |
| 网站分析 + 漏斗 | PostHog / Plausible（$0-450/月） |
| 会话回放 | PostHog / FullStory（$0-2000/月） |
| 错误追踪 | Sentry（$26+/月） |
| 链路追踪、指标与日志（OpenTelemetry） | Grafana Cloud / Datadog（$0-500+/月） |
| 可用性监控 | Better Uptime / Pingdom（$20+/月） |
| 托管 Postgres/Redis/S3 | AWS RDS / ElastiCache（$50+/月） |
| 事务性邮件 + DKIM | Resend / SendGrid（$20-100/月） |
| AI 代码执行沙箱 | E2B / Daytona / Vercel Sandbox（$150+/月 + 用量） |
| AI 网关 + 用量/成本追踪 | OpenRouter / Helicone / LangSmith（$0-200+/月） |
| 请求日志 + 代理 | Cloudflare（$0-200/月） |
| **使用 Temps 的总成本** | **$0（自托管）** |

---

## Temps 与其他方案对比

| 功能 | Temps | Coolify | Dokploy | Kamal | Railway | Render | Vercel |
|---|:---:|:---:|:---:|:---:|:---:|:---:|:---:|
| 自托管且开源 | 是 | 是 | 是 | 是 | 否 | 否 | 否 |
| 单一二进制安装 | 是 | 否 | 否 | CLI 工具 | -- | -- | -- |
| Git push 即部署 | 是 | 是 | 是 | 否 | 是 | 是 | 是 |
| 预览部署 | 是 | 是 | 是 | 否 | 是 | 是 | 是 |
| 自动 TLS（HTTP-01 + DNS-01） | 是 | 是 | 是 | 是 | 是 | 是 | 是 |
| Docker Compose 支持 | 是 | 是 | 是 | 否 | -- | -- | -- |
| 一键模板库 | 否 | 280+ | 是 | 否 | 是 | 是 | 是 |
| 网站分析 | 是 | 否 | 否 | 否 | 否 | 否 | 付费插件 |
| 会话回放 | 是 | 否 | 否 | 否 | 否 | 否 | 否 |
| 错误追踪（兼容 Sentry） | 是 | 否 | 否 | 否 | 否 | 否 | 否 |
| OpenTelemetry 链路追踪 + 指标 + 日志 | 是 | 否 | 否 | 否 | 否 | 否 | 链路追踪（付费） |
| 可用性监控 | 是 | 否 | 否 | 否 | 否 | 否 | 否 |
| 事务性邮件 + DKIM | 是 | 否 | 否 | 否 | 否 | 否 | 否 |
| 代码执行沙箱（API） | 是 | 否 | 否 | 否 | 否 | 否 | Sandbox（按用量计费） |
| AI 网关（自带密钥）+ 助手 | 是 | 否 | 否 | 否 | 否 | 否 | AI Gateway（付费） |
| 托管 Postgres / Redis | 是 | 是 | 是 | 否 | 是 | 是 | 合作方插件 |
| S3 兼容存储 | 是 | 否 | 否 | 否 | 否 | 否 | Blob（付费） |
| 多节点 / 集群 | 是 | 是 | Swarm | 是 | 平台托管 | 平台托管 | 平台托管 |
| 边缘函数 / 全球边缘网络 | 否 | 否 | 否 | 否 | 否 | 否 | 是 |
| 按席位收费 | 否 | 否 | 否 | 否 | $20/用户（Pro） | 按用户 | $20/席位（Pro） |

**这些替代方案的优势所在。** Coolify 和 Dokploy 拥有一键模板库（Coolify 上有 280+ 应用），这些 Temps 目前还没有；而且两者的社区规模远大于 Temps —— 仅 Coolify 就有 56k+ GitHub star，Temps 则是这份列表中最年轻的项目。如果你只需要通过 CLI 驱动的零停机 Docker 部署，Kamal 是更简单的选择。Vercel 和其他托管平台提供全球边缘网络、边缘函数和 DDoS 吸收能力，这些是单台 VPS 无法企及的 —— 而且它们替你运维基础设施，如果你完全不想操心服务器，这是实实在在的价值。

详细且持续更新的对比：[temps.sh/compare](https://temps.sh/compare?utm_source=github&utm_medium=repo&utm_content=compare_zh)

---

## 技术栈

- **后端：** Rust、Axum、Sea-ORM、Pingora（Cloudflare 的代理引擎）、Bollard（Docker API）
- **前端：** React 19、TypeScript、Tailwind CSS、shadcn/ui
- **数据库：** PostgreSQL + TimescaleDB
- **架构：** 30+ 个 workspace crate，三层服务架构

---

## SDK

| 包 | 说明 |
|---|---|
| [`@temps-sdk/node-sdk`](https://www.npmjs.com/package/@temps-sdk/node-sdk) | 平台 API 客户端 + 兼容 Sentry 的错误追踪 |
| [`@temps-sdk/react-analytics`](https://www.npmjs.com/package/@temps-sdk/react-analytics) | React 分析、会话回放、Web Vitals、参与度追踪 |
| [`@temps-sdk/kv`](https://www.npmjs.com/package/@temps-sdk/kv) | Serverless 键值存储 |
| [`@temps-sdk/blob`](https://www.npmjs.com/package/@temps-sdk/blob) | 文件存储（S3 兼容） |
| [`@temps-sdk/cli`](https://www.npmjs.com/package/@temps-sdk/cli) | 命令行工具 |

<details>
<summary><strong>快速示例</strong></summary>

**网站分析** —— 包裹你的 React 应用，其余一切自动完成：

```tsx
import { TempsAnalyticsProvider } from '@temps-sdk/react-analytics';

export default function App({ children }) {
  return <TempsAnalyticsProvider>{children}</TempsAnalyticsProvider>;
}
```

**错误追踪** —— 兼容 Sentry，可直接替换：

```typescript
import { ErrorTracking } from '@temps-sdk/node-sdk';

ErrorTracking.init({ dsn: 'https://key@your-instance.temps.dev/1' });

try {
  riskyOperation();
} catch (error) {
  ErrorTracking.captureException(error);
}
```

**KV 存储** —— 类 Redis API，零配置：

```typescript
import { kv } from '@temps-sdk/kv';

await kv.set('user:123', { name: 'Alice', plan: 'pro' }, { ex: 3600 });
const user = await kv.get('user:123');
```

**Blob 存储** —— 上传并托管文件：

```typescript
import { blob } from '@temps-sdk/blob';

const { url } = await blob.put('avatars/user-123.png', fileBuffer);
const files = await blob.list({ prefix: 'avatars/' });
```

</details>

---

## 社区

- [GitHub Discussions](https://github.com/gotempsh/temps/discussions) —— 提问、想法与作品展示
- [GitHub Issues](https://github.com/gotempsh/temps/issues) —— Bug 报告与功能请求

如果 Temps 帮你省下了一笔 SaaS 账单，[点个 star](https://github.com/gotempsh/temps) 能让更多人发现它。

---

## Star 历史

<a href="https://www.star-history.com/#gotempsh/temps&Date">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/svg?repos=gotempsh/temps&type=Date&theme=dark" />
    <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/svg?repos=gotempsh/temps&type=Date" />
    <img alt="Star 历史图表" src="https://api.star-history.com/svg?repos=gotempsh/temps&type=Date" />
  </picture>
</a>

---

## 参与贡献

欢迎贡献。参见 [CONTRIBUTING.md](CONTRIBUTING.md) 了解贡献指南。

```bash
git clone https://github.com/gotempsh/temps.git
cd temps
cargo build --release
```

---

## 许可证

采用 [MIT](LICENSE-MIT) 或 [Apache 2.0](LICENSE) 双许可证。

---

<div align="center">

[temps.sh](https://temps.sh/?utm_source=github&utm_medium=repo&utm_content=footer_zh) | [文档](https://temps.sh/docs?utm_source=github&utm_medium=repo&utm_content=footer_zh) | [GitHub](https://github.com/gotempsh/temps)

</div>
