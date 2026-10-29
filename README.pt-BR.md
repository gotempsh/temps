<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="web/public/logo/temps-logo-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="web/public/logo/temps-logo-light.png">
  <img alt="Temps" src="web/public/logo/temps-logo-dark.png" width="280">
</picture>

**A alternativa open source ao Vercel, Sentry, PostHog, Pingdom, Resend, E2B e Datadog, em um único binário auto-hospedado.**

[Site](https://temps.sh/?utm_source=github&utm_medium=repo&utm_content=header_pt-BR) · [Documentação](https://temps.sh/docs?utm_source=github&utm_medium=repo&utm_content=header_pt-BR) · [Discussões](https://github.com/gotempsh/temps/discussions)

</div>

---

Temps é um único binário auto-hospedado que faz o deploy das suas aplicações e roda tudo o que há em volta delas: bancos de dados, analytics, rastreamento de erros, traces, logs, uptime, email, sandboxes de IA e um gateway de IA. Open source, no seu próprio servidor.

## Instalação em uma VPS

```bash
curl -fsSL https://temps.sh/deploy.sh | bash
```

Execute em um servidor Ubuntu 24.04 ou 22.04 novo. Ele instala o Docker se estiver faltando, roda o PostgreSQL, configura o Temps como serviço com HTTPS automático e mostra a URL do console e a senha de administrador. Ainda sem domínio? Escolha o modo `quick` e você recebe `https://console.<ip-do-servidor>.sslip.io`.

[Início rápido](https://temps.sh/docs/quickstart?utm_source=github&utm_medium=repo&utm_content=install_pt-BR) · [Escolhendo um servidor](https://temps.sh/docs/choosing-a-server?utm_source=github&utm_medium=repo&utm_content=install_pt-BR) · Prefere não manter um servidor? [Temps Cloud](https://temps.sh/pricing?utm_source=github&utm_medium=repo&utm_content=cloud_cta_pt-BR)

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/dashboard-dark.png">
  <img alt="Dashboard de projetos do Temps — serviços agrupados em projetos, com tráfego, status e último deploy" src="assets/screenshots/dashboard-light.png">
</picture>

## Funcionalidades

- **Deploy com git push** — URLs de preview e rollouts sem downtime para qualquer linguagem, detectados automaticamente ou a partir do seu Dockerfile ou arquivo Docker Compose. [Docs](https://temps.sh/docs/deployments)
- **Bancos de dados gerenciados** — PostgreSQL, Redis, MongoDB, MariaDB e armazenamento compatível com S3, com backups. [Docs](https://temps.sh/docs/set-up-managed-services)
- **Domínios e TLS** — domínios próprios com certificados Let's Encrypt automáticos. [Docs](https://temps.sh/docs/domains-ssl)
- **Analytics web e session replay** — visitantes, páginas, funis e gravações, coletados pelo seu próprio servidor. [Docs](https://temps.sh/docs/analytics)
- **Rastreamento de erros** — compatível com Sentry: aponte o SDK do Sentry que você já usa para o Temps. [Docs](https://temps.sh/docs/error-tracking)
- **Traces, métricas e logs** — ingestão OpenTelemetry, traces entre serviços e logs de contêineres e requisições pesquisáveis. [Docs](https://temps.sh/docs/opentelemetry)
- **Uptime e alertas** — health checks, páginas de status e regras de alerta. [Docs](https://temps.sh/docs/monitoring)
- **Email transacional** — envio por SMTP ou SES com DKIM. [Docs](https://temps.sh/docs/transactional-email)
- **Sandboxes de IA** — sandboxes isolados com Docker ou Firecracker para executar código, com terminal e URLs de preview. [Docs](https://temps.sh/docs/sandboxes)
- **Gateway de IA** — um endpoint compatível com OpenAI para as suas próprias chaves de provedor, com uso e custo por modelo. [Docs](https://temps.sh/docs/ai-gateway)
- **Chat de IA** — pergunte sobre um projeto e receba respostas a partir dos próprios erros, logs e traces dele. Somente leitura, a menos que você permita alterações.
- **Importação** — traga aplicações, bancos de dados com os dados, domínios e variáveis de ambiente do Vercel, Netlify, Railway, Render, Fly.io, Coolify, Dokploy, CapRover, Portainer, Kamal ou Kubernetes. [Docs](https://temps.sh/docs/import-existing-platform)

<table>
  <tr>
    <td width="50%">
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/ai-chat-dark.png">
        <img alt="O chat de IA do Temps explicando checkouts com falha a partir dos erros, logs de requisição e traces do próprio projeto, com correções sugeridas" src="assets/screenshots/ai-chat-light.png">
      </picture>
    </td>
    <td width="50%">
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/errors-dark.png">
        <img alt="Rastreamento de erros do Temps — erros de todos os serviços com status, contagem de eventos e usuários, e um botão &quot;Fix with agent&quot;" src="assets/screenshots/errors-light.png">
      </picture>
    </td>
  </tr>
  <tr>
    <td width="50%">
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/traces-dark.png">
        <img alt="Trace distribuído no Temps — uma requisição de checkout passando por dois serviços, com o span que falhou e o erro dele" src="assets/screenshots/traces-light.png">
      </picture>
    </td>
    <td width="50%">
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/logs-dark.png">
        <img alt="Logs do Temps — volume de logs por nível, uma lista pesquisável de todos os serviços e contagens por nível e por serviço" src="assets/screenshots/logs-light.png">
      </picture>
    </td>
  </tr>
  <tr>
    <td width="50%">
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/analytics-dark.png">
        <img alt="Analytics web do Temps — visitantes, sessões e tráfego por hora" src="assets/screenshots/analytics-light.png">
      </picture>
    </td>
    <td width="50%">
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/sandbox-detail-dark.png">
        <img alt="Detalhe de sandbox no Temps — um sandbox Docker em execução com terminal ao vivo no navegador, tempo restante e modelo de URL de preview" src="assets/screenshots/sandbox-detail-light.png">
      </picture>
    </td>
  </tr>
</table>

## Comece com IA

Toda operação do console também é um comando da CLI, e o Temps traz um skill que ensina agentes de código (Claude Code, Codex, Cursor, OpenCode e outros) a usá-los. Conecte o seu e deixe que ele faça a configuração:

```bash
bunx skills add gotempsh/temps --skill temps
bunx @temps-sdk/cli login https://console.your-server.example
```

Depois peça ao seu agente:

> Use the Temps skill to verify my connection and list the services on this instance. Do not make any changes.

> Use the Temps skill to deploy this repository to Temps and tell me its URL when it is live.

Para um agente que roda sem supervisão, abra **Connect your coding agent** no console (`/setup/ai`): a página cria uma chave de API dedicada e confirma que a primeira requisição do agente chegou.

[Skills](https://temps.sh/docs/skills) · [CLI](https://temps.sh/docs/cli) · [Servidores MCP](https://temps.sh/docs/mcp-servers)

## Documentação

Todo o resto está na documentação em **[temps.sh/docs](https://temps.sh/docs?utm_source=github&utm_medium=repo&utm_content=docs_pt-BR)**:

- [Seu primeiro deploy](https://temps.sh/docs/your-first-deployment)
- [Importar de outra plataforma](https://temps.sh/docs/import-existing-platform)
- [SDKs](https://temps.sh/docs/sdks) — analytics, rastreamento de erros, armazenamento KV e blob
- [Checklist de produção](https://temps.sh/docs/production-checklist) e [atualização](https://temps.sh/docs/upgrade-temps)
- [Como o Temps se compara](https://temps.sh/compare?utm_source=github&utm_medium=repo&utm_content=compare_pt-BR) ao Vercel, Coolify, Dokploy e outros

## Telemetria

O Temps envia eventos de uso anônimos (um ID de instância aleatório, nomes de eventos como `deploy_succeeded` e contagens) para que os mantenedores saibam se as instalações auto-hospedadas funcionam. Ele nunca envia emails, IPs, nomes de repositórios, domínios ou qualquer coisa que você digitou, e os dados das suas aplicações nunca saem do seu servidor. Desative em **Settings › Telemetry**, ou para um host inteiro com `TEMPS_TELEMETRY=0`. [Detalhes](https://temps.sh/docs/data-ownership-and-privacy#anonymous-telemetry)

## Comunidade e contribuições

Perguntas e ideias nas [Discussões](https://github.com/gotempsh/temps/discussions), bugs nas [Issues](https://github.com/gotempsh/temps/issues). Contribuições são bem-vindas: veja [CONTRIBUTING.md](CONTRIBUTING.md).

## Licença

Licença dupla [MIT](LICENSE-MIT) ou [Apache 2.0](LICENSE). Forks e trabalhos derivados são bem-vindos, incluindo distribuições com outra marca e comerciais, desde que mantenham os avisos de copyright, licença e atribuição exigidos pela licença escolhida. Veja [NOTICE](NOTICE).

---

<div align="center">

[temps.sh](https://temps.sh/?utm_source=github&utm_medium=repo&utm_content=footer_pt-BR) | [Documentação](https://temps.sh/docs?utm_source=github&utm_medium=repo&utm_content=footer_pt-BR) | [GitHub](https://github.com/gotempsh/temps)

[English](README.md) | [简体中文](README.zh-CN.md) | [Español](README.es.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [日本語](README.ja.md) | Português

</div>
