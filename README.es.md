<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="web/public/logo/temps-logo-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="web/public/logo/temps-logo-light.png">
  <img alt="Temps" src="web/public/logo/temps-logo-dark.png" width="280">
</picture>

**La alternativa open source a Vercel, Sentry, PostHog, Pingdom, Resend, E2B y Datadog, en un solo binario autoalojado.**

[Web](https://temps.sh/?utm_source=github&utm_medium=repo&utm_content=header_es) · [Documentación](https://temps.sh/docs?utm_source=github&utm_medium=repo&utm_content=header_es) · [Discusiones](https://github.com/gotempsh/temps/discussions)

</div>

---

Temps es un único binario autoalojado que despliega tus aplicaciones y ejecuta todo lo que las rodea: bases de datos, analítica, seguimiento de errores, trazas, logs, uptime, email, sandboxes de IA y un gateway de IA. Open source, en tu propio servidor.

## Instalación en un VPS

```bash
curl -fsSL https://temps.sh/deploy.sh | bash
```

Ejecútalo en un servidor Ubuntu 24.04 o 22.04 recién creado. Instala Docker si falta, ejecuta PostgreSQL, configura Temps como servicio con HTTPS automático y muestra la URL de la consola y la contraseña de administrador. ¿Aún sin dominio? Elige el modo `quick` y obtendrás `https://console.<ip-del-servidor>.sslip.io`.

[Inicio rápido](https://temps.sh/docs/quickstart?utm_source=github&utm_medium=repo&utm_content=install_es) · [Elegir un servidor](https://temps.sh/docs/choosing-a-server?utm_source=github&utm_medium=repo&utm_content=install_es) · ¿Prefieres no gestionar un servidor? [Temps Cloud](https://temps.sh/pricing?utm_source=github&utm_medium=repo&utm_content=cloud_cta_es)

<a href="assets/screenshots/dashboard-light.png">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/dashboard-dark.png">
    <img alt="Panel de proyectos de Temps — servicios agrupados en proyectos, con tráfico, estado y último despliegue" src="assets/screenshots/dashboard-light.png">
  </picture>
</a><br>
<sub>Tamaño completo: <a href="assets/screenshots/dashboard-light.png">claro</a> · <a href="assets/screenshots/dashboard-dark.png">oscuro</a></sub>

## Funcionalidades

<table>
  <tr>
    <td width="50%" valign="top">
      <b>Despliegue con git push</b><br>
      URLs de vista previa y despliegues sin caída para cualquier lenguaje, detectados automáticamente o desde tu Dockerfile o archivo Docker Compose. <a href="https://temps.sh/docs/deployments">Docs</a><br><br>
      <a href="assets/screenshots/deployments-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/deployments-dark.png">
          <img alt="Despliegues en Temps — versiones de producción y ramas de vista previa con commit, autor y tiempo de build, y un build fallido con su causa" src="assets/screenshots/deployments-light.png">
        </picture>
      </a><br>
      <sub>Tamaño completo: <a href="assets/screenshots/deployments-light.png">claro</a> · <a href="assets/screenshots/deployments-dark.png">oscuro</a></sub>
    </td>
    <td width="50%" valign="top">
      <b>Bases de datos gestionadas</b><br>
      PostgreSQL, Redis, MongoDB, MariaDB y almacenamiento compatible con S3, con copias de seguridad. <a href="https://temps.sh/docs/set-up-managed-services">Docs</a><br><br>
      <a href="assets/screenshots/databases-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/databases-dark.png">
          <img alt="Bases de datos en Temps — servicios PostgreSQL, Redis, MongoDB, MariaDB y compatibles con S3 en ejecución, con CPU y memoria en vivo" src="assets/screenshots/databases-light.png">
        </picture>
      </a><br>
      <sub>Tamaño completo: <a href="assets/screenshots/databases-light.png">claro</a> · <a href="assets/screenshots/databases-dark.png">oscuro</a></sub>
    </td>
  </tr>
  <tr>
    <td width="50%" valign="top">
      <b>Dominios y TLS</b><br>
      Dominios propios y comodín con certificados Let's Encrypt automáticos. <a href="https://temps.sh/docs/domains-ssl">Docs</a><br><br>
      <a href="assets/screenshots/domains-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/domains-dark.png">
          <img alt="Dominios en Temps — cada dominio con su método HTTPS y la fecha de caducidad del certificado" src="assets/screenshots/domains-light.png">
        </picture>
      </a><br>
      <sub>Tamaño completo: <a href="assets/screenshots/domains-light.png">claro</a> · <a href="assets/screenshots/domains-dark.png">oscuro</a></sub>
    </td>
    <td width="50%" valign="top">
      <b>Analítica web</b><br>
      Visitantes, páginas, fuentes, embudos y rendimiento web, recogidos por tu propio servidor. <a href="https://temps.sh/docs/analytics">Docs</a><br><br>
      <a href="assets/screenshots/analytics-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/analytics-dark.png">
          <img alt="Analítica web de Temps — visitantes, sesiones y tráfico por hora" src="assets/screenshots/analytics-light.png">
        </picture>
      </a><br>
      <sub>Tamaño completo: <a href="assets/screenshots/analytics-light.png">claro</a> · <a href="assets/screenshots/analytics-dark.png">oscuro</a></sub>
    </td>
  </tr>
  <tr>
    <td width="50%" valign="top">
      <b>Session replay</b><br>
      Revive una visita tal como ocurrió, con los campos de formulario enmascarados por defecto. <a href="https://temps.sh/docs/session-replay">Docs</a><br><br>
      <a href="assets/screenshots/session-replay-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/session-replay-dark.png">
          <img alt="Session replay en Temps — una visita grabada a la tienda en el checkout, con campos enmascarados y la línea de eventos" src="assets/screenshots/session-replay-light.png">
        </picture>
      </a><br>
      <sub>Tamaño completo: <a href="assets/screenshots/session-replay-light.png">claro</a> · <a href="assets/screenshots/session-replay-dark.png">oscuro</a></sub>
    </td>
    <td width="50%" valign="top">
      <b>Seguimiento de errores</b><br>
      Compatible con Sentry: apunta tu SDK de Sentry actual a Temps y pasa un error a tu agente de código. <a href="https://temps.sh/docs/error-tracking">Docs</a><br><br>
      <a href="assets/screenshots/errors-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/errors-dark.png">
          <img alt="Seguimiento de errores de Temps — errores de todos los servicios con estado, número de eventos y usuarios, y un botón «Fix with agent»" src="assets/screenshots/errors-light.png">
        </picture>
      </a><br>
      <sub>Tamaño completo: <a href="assets/screenshots/errors-light.png">claro</a> · <a href="assets/screenshots/errors-dark.png">oscuro</a></sub>
    </td>
  </tr>
  <tr>
    <td width="50%" valign="top">
      <b>Trazas distribuidas</b><br>
      Ingesta OpenTelemetry, con trazas que siguen una petición entre servicios hasta el span que falla. <a href="https://temps.sh/docs/opentelemetry">Docs</a><br><br>
      <a href="assets/screenshots/traces-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/traces-dark.png">
          <img alt="Traza distribuida en Temps — una petición de checkout entre dos servicios, con el span que falla y su error" src="assets/screenshots/traces-light.png">
        </picture>
      </a><br>
      <sub>Tamaño completo: <a href="assets/screenshots/traces-light.png">claro</a> · <a href="assets/screenshots/traces-dark.png">oscuro</a></sub>
    </td>
    <td width="50%" valign="top">
      <b>Logs</b><br>
      Logs de contenedores y peticiones de todos los servicios en una sola vista con búsqueda, con volumen por nivel. <a href="https://temps.sh/docs/logs">Docs</a><br><br>
      <a href="assets/screenshots/logs-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/logs-dark.png">
          <img alt="Logs de Temps — volumen de logs por nivel, una lista con búsqueda de todos los servicios y recuentos por nivel y servicio" src="assets/screenshots/logs-light.png">
        </picture>
      </a><br>
      <sub>Tamaño completo: <a href="assets/screenshots/logs-light.png">claro</a> · <a href="assets/screenshots/logs-dark.png">oscuro</a></sub>
    </td>
  </tr>
  <tr>
    <td width="50%" valign="top">
      <b>Uptime y alertas</b><br>
      Comprobaciones de salud, páginas de estado y reglas de alerta. <a href="https://temps.sh/docs/monitoring">Docs</a><br><br>
      <a href="assets/screenshots/uptime-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/uptime-dark.png">
          <img alt="Uptime en Temps — estado actual, porcentaje de disponibilidad, tiempo medio de respuesta y una línea de estado por hora" src="assets/screenshots/uptime-light.png">
        </picture>
      </a><br>
      <sub>Tamaño completo: <a href="assets/screenshots/uptime-light.png">claro</a> · <a href="assets/screenshots/uptime-dark.png">oscuro</a></sub>
    </td>
    <td width="50%" valign="top">
      <b>Email transaccional</b><br>
      Envío por SMTP o SES con DKIM, y cada mensaje con su entrega, aperturas y clics. <a href="https://temps.sh/docs/transactional-email">Docs</a><br><br>
      <a href="assets/screenshots/email-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/email-dark.png">
          <img alt="Email en Temps — totales de enviados y fallidos, y mensajes recientes con su estado, aperturas y clics" src="assets/screenshots/email-light.png">
        </picture>
      </a><br>
      <sub>Tamaño completo: <a href="assets/screenshots/email-light.png">claro</a> · <a href="assets/screenshots/email-dark.png">oscuro</a></sub>
    </td>
  </tr>
  <tr>
    <td width="50%" valign="top">
      <b>Sandboxes de IA</b><br>
      Sandboxes aislados con Docker o Firecracker para ejecutar código, con terminal y URLs de vista previa. <a href="https://temps.sh/docs/sandboxes">Docs</a><br><br>
      <a href="assets/screenshots/sandbox-detail-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/sandbox-detail-dark.png">
          <img alt="Detalle de sandbox en Temps — un sandbox Docker en ejecución con terminal en vivo en el navegador, tiempo restante y plantilla de URL de vista previa" src="assets/screenshots/sandbox-detail-light.png">
        </picture>
      </a><br>
      <sub>Tamaño completo: <a href="assets/screenshots/sandbox-detail-light.png">claro</a> · <a href="assets/screenshots/sandbox-detail-dark.png">oscuro</a></sub>
    </td>
    <td width="50%" valign="top">
      <b>Gateway de IA</b><br>
      Un endpoint compatible con OpenAI para tus propias claves de proveedor, con uso y coste por modelo. <a href="https://temps.sh/docs/ai-gateway">Docs</a><br><br>
      <a href="assets/screenshots/ai-usage-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/ai-usage-dark.png">
          <img alt="Uso del gateway de IA en Temps — peticiones, tokens, latencia, tasa de error y coste estimado entre proveedores" src="assets/screenshots/ai-usage-light.png">
        </picture>
      </a><br>
      <sub>Tamaño completo: <a href="assets/screenshots/ai-usage-light.png">claro</a> · <a href="assets/screenshots/ai-usage-dark.png">oscuro</a></sub>
    </td>
  </tr>
  <tr>
    <td width="50%" valign="top">
      <b>Chat de IA</b><br>
      Pregunta por un proyecto y obtén respuestas a partir de sus propios errores, logs y trazas. Solo lectura salvo que permitas cambios.<br><br>
      <a href="assets/screenshots/ai-chat-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/ai-chat-dark.png">
          <img alt="El chat de IA de Temps explicando checkouts fallidos a partir de los errores, logs de peticiones y trazas del propio proyecto, con soluciones sugeridas" src="assets/screenshots/ai-chat-light.png">
        </picture>
      </a><br>
      <sub>Tamaño completo: <a href="assets/screenshots/ai-chat-light.png">claro</a> · <a href="assets/screenshots/ai-chat-dark.png">oscuro</a></sub>
    </td>
    <td width="50%" valign="top">
      <b>Importación</b><br>
      Trae aplicaciones, bases de datos con sus datos, dominios y variables de entorno desde Kubernetes, Coolify, Dokploy, CapRover, Portainer, Kamal o contenedores Docker, y desde Vercel con <code>temps migrate</code>. <a href="https://temps.sh/docs/import-existing-platform">Docs</a><br><br>
      <a href="assets/screenshots/import-light.png">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/import-dark.png">
          <img alt="Asistente de importación de Temps — elige el origen: Kubernetes, CapRover, Portainer, Kamal, Dokploy, Coolify o contenedores Docker" src="assets/screenshots/import-light.png">
        </picture>
      </a><br>
      <sub>Tamaño completo: <a href="assets/screenshots/import-light.png">claro</a> · <a href="assets/screenshots/import-dark.png">oscuro</a></sub>
    </td>
  </tr>
</table>

## Empieza con IA

Temps incluye skills que enseñan a los agentes de código (Claude Code, Codex, Cursor, OpenCode y otros) a instalar Temps en un servidor, conectarse a él y ejecutar cualquier operación de la consola desde la CLI. Añádelos a tu agente:

```bash
bunx skills add gotempsh/temps --skill temps temps-platform-setup
```

**¿Aún sin servidor?** Dale a tu agente un servidor Ubuntu recién creado al que pueda acceder por SSH y pídele:

> Use the Temps skills to install Temps on root@203.0.113.10 in quick mode, connect the CLI to it, then deploy this repository and tell me its URL when it is live.

Ejecuta el instalador oficial por SSH, deja en el servidor la contraseña de administrador y la clave de API generadas, y te da un enlace para aprobar el inicio de sesión de la CLI en el navegador, así que ninguna credencial pasa por el chat.

**¿Ya tienes Temps?** Conecta la CLI y pídele a tu agente:

```bash
bunx @temps-sdk/cli@0.1.38 login https://console.your-server.example
```

> Use the Temps skill to verify my connection and list the services on this instance. Do not make any changes.

> Use the Temps skill to deploy this repository to Temps and tell me its URL when it is live.

Para un agente que funciona sin supervisión, abre **Connect your coding agent** en la consola (`/setup/ai`): crea una clave de API dedicada y comprueba que llega la primera petición del agente.

[Skills](https://temps.sh/docs/skills) · [CLI](https://temps.sh/docs/cli) · [Servidores MCP](https://temps.sh/docs/mcp-servers)

## Documentación

Todo lo demás está en la documentación en **[temps.sh/docs](https://temps.sh/docs?utm_source=github&utm_medium=repo&utm_content=docs_es)**:

- [Tu primer despliegue](https://temps.sh/docs/your-first-deployment)
- [Importar desde otra plataforma](https://temps.sh/docs/import-existing-platform)
- [SDKs](https://temps.sh/docs/sdks) — analítica, seguimiento de errores, almacenamiento KV y blob
- [Lista de comprobación para producción](https://temps.sh/docs/production-checklist) y [actualizar](https://temps.sh/docs/upgrade-temps)
- [Cómo se compara Temps](https://temps.sh/compare?utm_source=github&utm_medium=repo&utm_content=compare_es) con Vercel, Coolify, Dokploy y otros

## Telemetría

Temps envía eventos de uso anónimos (un ID de instancia aleatorio, nombres de evento como `deploy_succeeded` y recuentos) para que los mantenedores sepan si las instalaciones autoalojadas funcionan. Nunca envía emails, IPs, nombres de repositorio, dominios ni nada que hayas escrito, y los datos de tus aplicaciones nunca salen de tu servidor. Desactívala en **Settings › Telemetry**, o para todo un host con `TEMPS_TELEMETRY=0`. [Detalles](https://temps.sh/docs/data-ownership-and-privacy#anonymous-telemetry)

## Comunidad y contribuciones

Preguntas e ideas en [Discusiones](https://github.com/gotempsh/temps/discussions), errores en [Issues](https://github.com/gotempsh/temps/issues). Las contribuciones son bienvenidas: consulta [CONTRIBUTING.md](CONTRIBUTING.md).

## Licencia

Doble licencia [MIT](LICENSE-MIT) o [Apache 2.0](LICENSE). Los forks y trabajos derivados son bienvenidos, incluidas distribuciones con otra marca y comerciales, siempre que conserven los avisos de copyright, licencia y atribución que exija la licencia elegida. Consulta [NOTICE](NOTICE).

---

<div align="center">

[temps.sh](https://temps.sh/?utm_source=github&utm_medium=repo&utm_content=footer_es) | [Documentación](https://temps.sh/docs?utm_source=github&utm_medium=repo&utm_content=footer_es) | [GitHub](https://github.com/gotempsh/temps)

[English](README.md) | [简体中文](README.zh-CN.md) | Español | [Français](README.fr.md) | [Deutsch](README.de.md) | [日本語](README.ja.md) | [Português](README.pt-BR.md)

</div>
