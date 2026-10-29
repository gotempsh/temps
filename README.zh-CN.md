<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="web/public/logo/temps-logo-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="web/public/logo/temps-logo-light.png">
  <img alt="Temps" src="web/public/logo/temps-logo-dark.png" width="280">
</picture>

**Vercel、Sentry、PostHog、Pingdom、Resend、E2B 和 Datadog 的开源替代方案，一个可自托管的二进制文件。**

[官网](https://temps.sh/?utm_source=github&utm_medium=repo&utm_content=header_zh) · [文档](https://temps.sh/docs?utm_source=github&utm_medium=repo&utm_content=header_zh) · [讨论区](https://github.com/gotempsh/temps/discussions)

</div>

---

Temps 是一个可自托管的单一二进制文件，负责部署你的应用并运行其周边的一切：数据库、网站分析、错误追踪、链路追踪、日志、可用性监控、邮件、AI 沙箱和 AI 网关。开源，运行在你自己的服务器上。

## 在 VPS 上安装

```bash
curl -fsSL https://temps.sh/deploy.sh | bash
```

在一台全新的 Ubuntu 24.04 或 22.04 服务器上运行。它会在缺少 Docker 时自动安装，运行 PostgreSQL，把 Temps 设置为带自动 HTTPS 的系统服务，并输出控制台地址和管理员密码。还没有域名？选择 `quick` 模式即可获得 `https://console.<服务器IP>.sslip.io`。

[快速开始](https://temps.sh/docs/quickstart?utm_source=github&utm_medium=repo&utm_content=install_zh) · [如何选择服务器](https://temps.sh/docs/choosing-a-server?utm_source=github&utm_medium=repo&utm_content=install_zh) · 不想自己维护服务器？ [Temps Cloud](https://temps.sh/pricing?utm_source=github&utm_medium=repo&utm_content=cloud_cta_zh)

<a href="assets/screenshots/dashboard-light.png">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/dashboard-dark.png">
    <img alt="Temps 项目仪表盘 —— 按项目分组的服务，含流量、状态与最近一次部署" src="assets/screenshots/dashboard-light.png">
  </picture>
</a><br>
<sub>查看原图：<a href="assets/screenshots/dashboard-light.png">浅色</a> · <a href="assets/screenshots/dashboard-dark.png">深色</a></sub>

## 功能

<table>
  <tr>
    <td width="50%" valign="top">
      <b>git push 即部署</b><br>
      任何语言都有预览 URL 和零停机发布，自动识别，或使用你的 Dockerfile 或 Docker Compose 文件。 <a href="https://temps.sh/docs/deployments">文档</a><br><br>
      <a href="assets/screenshots/deployments-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/deployments-dark.png">
          <img alt="Temps 部署列表 —— 生产发布和预览分支，含提交、作者和构建耗时，以及一次带失败原因的构建" src="assets/screenshots/deployments-light.png">
        </picture>
      </a><br>
      <sub>查看原图：<a href="assets/screenshots/deployments-light.png">浅色</a> · <a href="assets/screenshots/deployments-dark.png">深色</a></sub>
    </td>
    <td width="50%" valign="top">
      <b>托管数据库</b><br>
      PostgreSQL、Redis、MongoDB、MariaDB 和 S3 兼容存储，自带备份。 <a href="https://temps.sh/docs/set-up-managed-services">文档</a><br><br>
      <a href="assets/screenshots/databases-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/databases-dark.png">
          <img alt="Temps 数据库 —— 正在运行的 PostgreSQL、Redis、MongoDB、MariaDB 和 S3 兼容服务，显示实时 CPU 和内存" src="assets/screenshots/databases-light.png">
        </picture>
      </a><br>
      <sub>查看原图：<a href="assets/screenshots/databases-light.png">浅色</a> · <a href="assets/screenshots/databases-dark.png">深色</a></sub>
    </td>
  </tr>
  <tr>
    <td width="50%" valign="top">
      <b>域名与 TLS</b><br>
      自定义域名和通配符域名，自动签发 Let's Encrypt 证书。 <a href="https://temps.sh/docs/domains-ssl">文档</a><br><br>
      <a href="assets/screenshots/domains-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/domains-dark.png">
          <img alt="Temps 域名 —— 每个域名的 HTTPS 方式和证书到期日期" src="assets/screenshots/domains-light.png">
        </picture>
      </a><br>
      <sub>查看原图：<a href="assets/screenshots/domains-light.png">浅色</a> · <a href="assets/screenshots/domains-dark.png">深色</a></sub>
    </td>
    <td width="50%" valign="top">
      <b>网站分析</b><br>
      访客、页面、来源、漏斗和网页性能，由你自己的服务器收集。 <a href="https://temps.sh/docs/analytics">文档</a><br><br>
      <a href="assets/screenshots/analytics-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/analytics-dark.png">
          <img alt="Temps 网站分析 —— 访客、会话和每小时流量" src="assets/screenshots/analytics-light.png">
        </picture>
      </a><br>
      <sub>查看原图：<a href="assets/screenshots/analytics-light.png">浅色</a> · <a href="assets/screenshots/analytics-dark.png">深色</a></sub>
    </td>
  </tr>
  <tr>
    <td width="50%" valign="top">
      <b>会话回放</b><br>
      按原样回看一次访问，表单输入默认被遮盖。 <a href="https://temps.sh/docs/session-replay">文档</a><br><br>
      <a href="assets/screenshots/session-replay-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/session-replay-dark.png">
          <img alt="Temps 会话回放 —— 一次录制的商店结账访问，输入已遮盖，并附事件时间线" src="assets/screenshots/session-replay-light.png">
        </picture>
      </a><br>
      <sub>查看原图：<a href="assets/screenshots/session-replay-light.png">浅色</a> · <a href="assets/screenshots/session-replay-dark.png">深色</a></sub>
    </td>
    <td width="50%" valign="top">
      <b>错误追踪</b><br>
      兼容 Sentry：把现有的 Sentry SDK 指向 Temps，再把错误交给你的编程智能体。 <a href="https://temps.sh/docs/error-tracking">文档</a><br><br>
      <a href="assets/screenshots/errors-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/errors-dark.png">
          <img alt="Temps 错误追踪 —— 各服务的错误及其状态、事件数和用户数，并带有「Fix with agent」按钮" src="assets/screenshots/errors-light.png">
        </picture>
      </a><br>
      <sub>查看原图：<a href="assets/screenshots/errors-light.png">浅色</a> · <a href="assets/screenshots/errors-dark.png">深色</a></sub>
    </td>
  </tr>
  <tr>
    <td width="50%" valign="top">
      <b>分布式追踪</b><br>
      接入 OpenTelemetry，追踪一个请求跨服务直到出错的 span。 <a href="https://temps.sh/docs/opentelemetry">文档</a><br><br>
      <a href="assets/screenshots/traces-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/traces-dark.png">
          <img alt="Temps 分布式追踪 —— 一个跨两个服务的结账请求，以及出错的 span 和错误信息" src="assets/screenshots/traces-light.png">
        </picture>
      </a><br>
      <sub>查看原图：<a href="assets/screenshots/traces-light.png">浅色</a> · <a href="assets/screenshots/traces-dark.png">深色</a></sub>
    </td>
    <td width="50%" valign="top">
      <b>日志</b><br>
      所有服务的容器日志和请求日志集中在一个可搜索的视图中，并按级别显示日志量。 <a href="https://temps.sh/docs/logs">文档</a><br><br>
      <a href="assets/screenshots/logs-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/logs-dark.png">
          <img alt="Temps 日志 —— 按级别的日志量、跨服务可搜索的列表，以及按级别和服务的计数" src="assets/screenshots/logs-light.png">
        </picture>
      </a><br>
      <sub>查看原图：<a href="assets/screenshots/logs-light.png">浅色</a> · <a href="assets/screenshots/logs-dark.png">深色</a></sub>
    </td>
  </tr>
  <tr>
    <td width="50%" valign="top">
      <b>在线监控与告警</b><br>
      健康检查、状态页和告警规则。 <a href="https://temps.sh/docs/monitoring">文档</a><br><br>
      <a href="assets/screenshots/uptime-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/uptime-dark.png">
          <img alt="Temps 在线监控 —— 当前状态、可用率、平均响应时间和按小时的状态时间线" src="assets/screenshots/uptime-light.png">
        </picture>
      </a><br>
      <sub>查看原图：<a href="assets/screenshots/uptime-light.png">浅色</a> · <a href="assets/screenshots/uptime-dark.png">深色</a></sub>
    </td>
    <td width="50%" valign="top">
      <b>事务邮件</b><br>
      通过 SMTP 或 SES 发送并支持 DKIM，查看每封邮件的投递、打开和点击。 <a href="https://temps.sh/docs/transactional-email">文档</a><br><br>
      <a href="assets/screenshots/email-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/email-dark.png">
          <img alt="Temps 邮件 —— 已发送和失败总数，以及最近邮件的状态、打开和点击" src="assets/screenshots/email-light.png">
        </picture>
      </a><br>
      <sub>查看原图：<a href="assets/screenshots/email-light.png">浅色</a> · <a href="assets/screenshots/email-dark.png">深色</a></sub>
    </td>
  </tr>
  <tr>
    <td width="50%" valign="top">
      <b>AI 沙箱</b><br>
      用于运行代码的隔离 Docker 或 Firecracker 沙箱，带终端和预览 URL。 <a href="https://temps.sh/docs/sandboxes">文档</a><br><br>
      <a href="assets/screenshots/sandbox-detail-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/sandbox-detail-dark.png">
          <img alt="Temps 沙箱详情 —— 正在运行的 Docker 沙箱，带浏览器内实时终端、剩余时间和预览 URL 模板" src="assets/screenshots/sandbox-detail-light.png">
        </picture>
      </a><br>
      <sub>查看原图：<a href="assets/screenshots/sandbox-detail-light.png">浅色</a> · <a href="assets/screenshots/sandbox-detail-dark.png">深色</a></sub>
    </td>
    <td width="50%" valign="top">
      <b>AI 网关</b><br>
      为你自己的提供商密钥提供一个兼容 OpenAI 的端点，并按模型统计用量和费用。 <a href="https://temps.sh/docs/ai-gateway">文档</a><br><br>
      <a href="assets/screenshots/ai-usage-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/ai-usage-dark.png">
          <img alt="Temps AI 网关用量 —— 跨提供商的请求数、Token、延迟、错误率和预估费用" src="assets/screenshots/ai-usage-light.png">
        </picture>
      </a><br>
      <sub>查看原图：<a href="assets/screenshots/ai-usage-light.png">浅色</a> · <a href="assets/screenshots/ai-usage-dark.png">深色</a></sub>
    </td>
  </tr>
  <tr>
    <td width="50%" valign="top">
      <b>AI 对话</b><br>
      就某个项目提问，答案来自它自己的错误、日志和追踪。除非你允许修改，否则只读。<br><br>
      <a href="assets/screenshots/ai-chat-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/ai-chat-dark.png">
          <img alt="Temps AI 对话根据项目自身的错误、请求日志和追踪解释结账失败的原因，并给出修复建议" src="assets/screenshots/ai-chat-light.png">
        </picture>
      </a><br>
      <sub>查看原图：<a href="assets/screenshots/ai-chat-light.png">浅色</a> · <a href="assets/screenshots/ai-chat-dark.png">深色</a></sub>
    </td>
    <td width="50%" valign="top">
      <b>导入</b><br>
      从 Kubernetes、Coolify、Dokploy、CapRover、Portainer、Kamal 或普通 Docker 容器导入应用、连同数据的数据库、域名和环境变量；从 Vercel 导入请使用 <code>temps migrate</code>。 <a href="https://temps.sh/docs/import-existing-platform">文档</a><br><br>
      <a href="assets/screenshots/import-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/import-dark.png">
          <img alt="Temps 导入向导 —— 选择来源：Kubernetes、CapRover、Portainer、Kamal、Dokploy、Coolify 或 Docker 容器" src="assets/screenshots/import-light.png">
        </picture>
      </a><br>
      <sub>查看原图：<a href="assets/screenshots/import-light.png">浅色</a> · <a href="assets/screenshots/import-dark.png">深色</a></sub>
    </td>
  </tr>
</table>

## 用 AI 上手

Temps 自带 skills，教编程智能体（Claude Code、Codex、Cursor、OpenCode 等）在服务器上安装 Temps、连接它，并通过 CLI 完成控制台里的任何操作。把它们添加到你的智能体：

```bash
bunx skills add gotempsh/temps --skill temps temps-platform-setup
```

**还没有服务器？** 给智能体一台可以通过 SSH 访问的全新 Ubuntu 服务器，然后这样说：

> Use the Temps skills to install Temps on root@203.0.113.10 in quick mode, connect the CLI to it, then deploy this repository and tell me its URL when it is live.

它会通过 SSH 运行官方安装脚本，把生成的管理员密码和 API 密钥留在服务器上，并给你一个在浏览器中批准 CLI 登录的链接，任何凭据都不会经过聊天。

**已经在运行 Temps？** 连接 CLI，然后这样问你的智能体：

```bash
bunx @temps-sdk/cli@0.1.38 login https://console.your-server.example
```

> Use the Temps skill to verify my connection and list the services on this instance. Do not make any changes.

> Use the Temps skill to deploy this repository to Temps and tell me its URL when it is live.

对于无人值守运行的智能体，请在控制台打开 **Connect your coding agent**（`/setup/ai`）：它会创建专用的 API 密钥，并确认智能体的第一个请求已经到达。

[Skills](https://temps.sh/docs/skills) · [CLI](https://temps.sh/docs/cli) · [MCP 服务器](https://temps.sh/docs/mcp-servers)

## 文档

其余内容都在文档中： **[temps.sh/docs](https://temps.sh/docs?utm_source=github&utm_medium=repo&utm_content=docs_zh)**:

- [你的第一次部署](https://temps.sh/docs/your-first-deployment)
- [从其他平台导入](https://temps.sh/docs/import-existing-platform)
- [SDK](https://temps.sh/docs/sdks) —— 网站分析、错误追踪、KV 与 Blob 存储
- [生产环境检查清单](https://temps.sh/docs/production-checklist) 与 [升级](https://temps.sh/docs/upgrade-temps)
- [Temps 与其他方案的对比](https://temps.sh/compare?utm_source=github&utm_medium=repo&utm_content=compare_zh) （Vercel、Coolify、Dokploy 等）

## 遥测

Temps 会发送匿名使用事件（一个随机的实例 ID、`deploy_succeeded` 之类的事件名和计数），让维护者了解自托管安装是否正常工作。它从不发送邮箱、IP、仓库名、域名或任何你输入的内容，你的应用数据也永远不会离开你的服务器。可在 **Settings › Telemetry** 中关闭，或用 `TEMPS_TELEMETRY=0` 为整台主机关闭。 [详情](https://temps.sh/docs/data-ownership-and-privacy#anonymous-telemetry)

## 社区与贡献

问题和想法请到 [讨论区](https://github.com/gotempsh/temps/discussions)，bug 请提交到 [Issues](https://github.com/gotempsh/temps/issues)。欢迎贡献：请参阅 [CONTRIBUTING.md](CONTRIBUTING.md)。

## 许可证

采用 [MIT](LICENSE-MIT) 或 [Apache 2.0](LICENSE) 双许可证。欢迎 fork 和衍生作品，包括更换品牌和商业发行版，但须保留所选许可证要求的版权、许可证和署名声明。参见 [NOTICE](NOTICE)。

---

<div align="center">

[temps.sh](https://temps.sh/?utm_source=github&utm_medium=repo&utm_content=footer_zh) | [文档](https://temps.sh/docs?utm_source=github&utm_medium=repo&utm_content=footer_zh) | [GitHub](https://github.com/gotempsh/temps)

[English](README.md) | 简体中文 | [Español](README.es.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [日本語](README.ja.md) | [Português](README.pt-BR.md)

</div>
