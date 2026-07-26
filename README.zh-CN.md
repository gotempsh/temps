<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="web/public/logo/temps-logo-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="web/public/logo/temps-logo-light.png">
  <img alt="Temps" src="web/public/logo/temps-logo-dark.png" width="280">
</picture>

**Vercel + Sentry + PostHog + Pingdom 的开源替代品。**
部署、分析、会话回放、错误追踪 —— 一个自托管的二进制文件搞定。

[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE)
[![GitHub Release](https://img.shields.io/github/v/release/gotempsh/temps)](https://github.com/gotempsh/temps/releases)
[![Rust](https://img.shields.io/badge/rust-1.70%2B-orange.svg)](https://www.rust-lang.org/)
[![GitHub Stars](https://img.shields.io/github/stars/gotempsh/temps?style=social)](https://github.com/gotempsh/temps)

[官网](https://temps.sh) · [文档](https://temps.sh/docs) · [快速开始](https://temps.sh/docs/introduction) · [讨论区](https://github.com/gotempsh/temps/discussions) · [Contributing](CONTRIBUTING.md)

[English](README.md) | 简体中文 | [Español](README.es.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [日本語](README.ja.md) | [Português](README.pt-BR.md)

</div>

---

```bash
curl -fsSL https://temps.sh/deploy.sh | bash
```

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/readme-hero-dark.png">
  <img alt="Temps — analytics, uptime, error tracking, deployments, request logs, dashboard" src="assets/readme-hero-light.png">
</picture>


别再为 6 个不同的 SaaS 工具付费了。Temps 一次性替代你的部署平台、网站分析、错误追踪、会话回放、可用性监控和事务性邮件 —— 全部自托管，全部集成在一个二进制文件里。

---

## 功能特性

<table>
<tr>
<td width="50%">

**内置网站分析与会话回放**
带漏斗分析、访客追踪和会话回放（rrweb）的网站分析，外加兼容 Sentry 的错误追踪。无需任何外部服务 —— 这是其他自托管 PaaS 都没有的能力。


</td>
<td width="50%">

**可用性监控与告警**
带状态时间线的可用性监控，以及针对部署失败、运行时崩溃、证书过期和备份健康状况的告警。在问题波及用户之前就收到通知。


</td>
</tr>
<tr>
<td width="50%">

**Git Push 即部署**
推送到 Git，Temps 自动构建并部署。自动检测框架、生成预览 URL，并完成零停机发布。


</td>
<td width="50%">

**一个仪表盘掌握全局**
每个项目的访客、错误、部署状态和监控健康状况 —— 一个页面搞定，不用再开六个浏览器标签页。


</td>
</tr>
<tr>
<td width="50%">

**基于 Pingora 的代理**
运行在 Cloudflare 的 Pingora 引擎之上。通过 Let's Encrypt 自动签发 TLS 证书（HTTP-01 和 DNS-01）、支持自定义域名和完整的请求日志。


</td>
<td width="50%">

**请求日志与代理可观测性**
每个 HTTP 请求都会记录方法、路径、状态码、响应时间和路由元数据。无需额外工具即可过滤和搜索。


</td>
</tr>
<tr>
<td width="100%" colspan="2">

**托管服务与事务性邮件**
在应用旁边一键开通 Postgres、Redis、S3（MinIO）和 MongoDB —— 创建、备份和销毁都由 Temps 负责。通过 UI 添加带 DKIM 记录的发件域名，并通过 `@temps-sdk/node-sdk` 发送事务性邮件。无需任何外部服务。

</td>
</tr>
</table>

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

## 快速开始

```bash
curl -fsSL https://temps.sh/deploy.sh | bash
```

**已测试系统：** Ubuntu 24.04 / 22.04 &nbsp;|&nbsp; macOS 也可运行

不想自己维护服务器？[Temps Cloud](https://temps.sh/pricing) 可以在托管基础设施上为你运行 Temps。

---

## Temps 能替代什么

| 你得到的 | 不用再付费的 |
|---|---|
| Git 部署 + 预览 URL | Vercel / Netlify / Railway（$20+/月） |
| 网站分析 + 漏斗 | PostHog / Plausible（$0-450/月） |
| 会话回放 | PostHog / FullStory（$0-2000/月） |
| 错误追踪 | Sentry（$26+/月） |
| 可用性监控 | Better Uptime / Pingdom（$20+/月） |
| 托管 Postgres/Redis/S3 | AWS RDS / ElastiCache（$50+/月） |
| 事务性邮件 + DKIM | Resend / SendGrid（$20-100/月） |
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
| Docker Compose 支持 | 否 | 是 | 是 | 否 | -- | -- | -- |
| 一键模板库 | 否 | 280+ | 是 | 否 | 是 | 是 | 是 |
| 网站分析 | 是 | 否 | 否 | 否 | 否 | 否 | 付费插件 |
| 会话回放 | 是 | 否 | 否 | 否 | 否 | 否 | 否 |
| 错误追踪（兼容 Sentry） | 是 | 否 | 否 | 否 | 否 | 否 | 否 |
| 可用性监控 | 是 | 否 | 否 | 否 | 否 | 否 | 否 |
| 事务性邮件 + DKIM | 是 | 否 | 否 | 否 | 否 | 否 | 否 |
| 托管 Postgres / Redis | 是 | 是 | 是 | 否 | 是 | 是 | 合作方插件 |
| S3 兼容存储 | 是 | 否 | 否 | 否 | 否 | 否 | Blob（付费） |
| 多节点 / 集群 | 是 | 是 | Swarm | 是 | 平台托管 | 平台托管 | 平台托管 |
| 边缘函数 / 全球边缘网络 | 否 | 否 | 否 | 否 | 否 | 否 | 是 |
| 按席位收费 | 否 | 否 | 否 | 否 | $20/用户（Pro） | 按用户 | $20/席位（Pro） |

**这些替代方案的优势所在。** Coolify 和 Dokploy 拥有一流的 Docker Compose 支持和一键模板库（Coolify 上有 280+ 应用），这些 Temps 目前还没有；而且两者的社区规模远大于 Temps —— 仅 Coolify 就有 56k+ GitHub star，Temps 则是这份列表中最年轻的项目。如果你只需要通过 CLI 驱动的零停机 Docker 部署，Kamal 是更简单的选择。Vercel 和其他托管平台提供全球边缘网络、边缘函数和 DDoS 吸收能力，这些是单台 VPS 无法企及的 —— 而且它们替你运维基础设施，如果你完全不想操心服务器，这是实实在在的价值。

详细且持续更新的对比：[temps.sh/compare](https://temps.sh/compare)

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

[temps.sh](https://temps.sh) | [文档](https://temps.sh/docs) | [GitHub](https://github.com/gotempsh/temps)

</div>
