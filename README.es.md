<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="web/public/logo/temps-logo-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="web/public/logo/temps-logo-light.png">
  <img alt="Temps" src="web/public/logo/temps-logo-dark.png" width="280">
</picture>

### La plataforma de despliegue open source y autoalojada.
### Despliega, observa y escala -- desde un único binario.

[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE)
[![GitHub Release](https://img.shields.io/github/v/release/gotempsh/temps)](https://github.com/gotempsh/temps/releases)
[![Rust](https://img.shields.io/badge/rust-1.70%2B-orange.svg)](https://www.rust-lang.org/)
[![GitHub Stars](https://img.shields.io/github/stars/gotempsh/temps?style=social)](https://github.com/gotempsh/temps)

[Sitio web](https://temps.sh) | [Documentación](https://temps.sh/docs) | [Inicio rápido](https://temps.sh/docs/introduction) | [Discusiones](https://github.com/gotempsh/temps/discussions)

[English](README.md) | [简体中文](README.zh-CN.md) | Español | [Français](README.fr.md) | [Deutsch](README.de.md) | [日本語](README.ja.md) | [Português](README.pt-BR.md)

</div>

---

<p align="center">
  <img src="temps-demo.gif" alt="Temps — de un servidor vacío a desplegado en menos de 3 minutos" width="800" />
  <br />
  <em>De un servidor vacío a todo desplegado — en menos de 3 minutos (166 s).</em>
</p>

```bash
curl -fsSL https://temps.sh/deploy.sh | bash
```

![Panel de Temps](assets/screenshots/dashboard.png)

Deja de pagar 6 herramientas SaaS distintas. Temps sustituye tu plataforma de despliegue, analítica web, seguimiento de errores, reproducción de sesiones, monitorización de disponibilidad y email transaccional -- todo autoalojado, todo en un solo binario.

---

## Características

<table>
<tr>
<td width="50%">

**Analítica y reproducción de sesiones integradas**
Analítica web con embudos, seguimiento de visitantes y reproducción de sesiones (rrweb). Seguimiento de errores compatible con Sentry. Sin servicios externos — esto es lo que ningún otro PaaS autoalojado tiene.

![Analítica](assets/screenshots/analytics.png)

</td>
<td width="50%">

**Monitorización de disponibilidad y alertas**
Monitores de disponibilidad con líneas de tiempo de estado, además de alertas por fallos de despliegue, caídas en tiempo de ejecución, expiración de certificados y salud de las copias de seguridad. Entérate antes de que los problemas lleguen a tus usuarios.

![Monitorización de disponibilidad](assets/screenshots/monitoring-detail.png)

</td>
</tr>
<tr>
<td width="50%">

**Git push para desplegar**
Haz push a Git y Temps construye y despliega. Detecta frameworks automáticamente, crea URLs de vista previa y gestiona despliegues sin tiempo de inactividad.

![Despliegues](assets/screenshots/deployments.png)

</td>
<td width="50%">

**Todo en un solo panel**
Visitantes, errores, estado de despliegues y salud de la monitorización por proyecto — un solo lugar en vez de seis pestañas del navegador.

![Vista general del proyecto](assets/screenshots/project-overview.png)

</td>
</tr>
<tr>
<td width="50%">

**Proxy impulsado por Pingora**
Funciona sobre el motor Pingora de Cloudflare. TLS automático vía Let's Encrypt (HTTP-01 y DNS-01), dominios personalizados y registro completo de peticiones.

![Dominios](assets/screenshots/domains.png)

</td>
<td width="50%">

**Registro de peticiones y visibilidad del proxy**
Cada petición HTTP queda registrada con método, ruta, estado, tiempo de respuesta y metadatos de enrutamiento. Filtra y busca sin herramientas adicionales.

![Registros del proxy](assets/screenshots/proxy-logs.png)

</td>
</tr>
<tr>
<td width="100%" colspan="2">

**Servicios gestionados y email transaccional**
Aprovisiona Postgres, Redis, S3 (MinIO) y MongoDB junto a tus aplicaciones — Temps se encarga de la creación, las copias de seguridad y el desmantelamiento. Añade dominios remitentes con registros DKIM desde la interfaz y envía email transaccional con `@temps-sdk/node-sdk`. Sin necesidad de servicios externos.

</td>
</tr>
</table>

### Funciona con tu stack

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

<p align="center"><em>Cualquier lenguaje, cualquier framework. Detección automática o trae tu propio Dockerfile.</em></p>

---

## Inicio rápido

```bash
curl -fsSL https://temps.sh/deploy.sh | bash
```

**Probado en:** Ubuntu 24.04 / 22.04 &nbsp;|&nbsp; También funciona en macOS

¿Prefieres no gestionar un servidor? [Temps Cloud](https://temps.sh/pricing) ejecuta Temps por ti en infraestructura gestionada.

---

## Qué sustituye Temps

| Lo que obtienes | En lugar de pagar por |
|---|---|
| Despliegues desde Git + URLs de vista previa | Vercel / Netlify / Railway ($20+/mes) |
| Analítica web + embudos | PostHog / Plausible ($0-450/mes) |
| Reproducción de sesiones | PostHog / FullStory ($0-2000/mes) |
| Seguimiento de errores | Sentry ($26+/mes) |
| Monitorización de disponibilidad | Better Uptime / Pingdom ($20+/mes) |
| Postgres/Redis/S3 gestionados | AWS RDS / ElastiCache ($50+/mes) |
| Email transaccional + DKIM | Resend / SendGrid ($20-100/mes) |
| Registro de peticiones + proxy | Cloudflare ($0-200/mes) |
| **Total con Temps** | **$0 (autoalojado)** |

---

## Temps frente a las alternativas

| Característica | Temps | Coolify | Dokploy | Kamal | Railway | Render | Vercel |
|---|:---:|:---:|:---:|:---:|:---:|:---:|:---:|
| Autoalojado y open source | Sí | Sí | Sí | Sí | No | No | No |
| Instalación con un solo binario | Sí | No | No | Herramienta CLI | -- | -- | -- |
| Despliegue con git push | Sí | Sí | Sí | No | Sí | Sí | Sí |
| Despliegues de vista previa | Sí | Sí | Sí | No | Sí | Sí | Sí |
| TLS automático (HTTP-01 + DNS-01) | Sí | Sí | Sí | Sí | Sí | Sí | Sí |
| Soporte de Docker Compose | No | Sí | Sí | No | -- | -- | -- |
| Biblioteca de plantillas con un clic | No | 280+ | Sí | No | Sí | Sí | Sí |
| Analítica web | Sí | No | No | No | No | No | Complemento de pago |
| Reproducción de sesiones | Sí | No | No | No | No | No | No |
| Seguimiento de errores (compatible con Sentry) | Sí | No | No | No | No | No | No |
| Monitorización de disponibilidad | Sí | No | No | No | No | No | No |
| Email transaccional + DKIM | Sí | No | No | No | No | No | No |
| Postgres / Redis gestionados | Sí | Sí | Sí | No | Sí | Sí | Complementos de partners |
| Almacenamiento compatible con S3 | Sí | No | No | No | No | No | Blob (de pago) |
| Multinodo / clustering | Sí | Sí | Swarm | Sí | Gestionado | Gestionado | Gestionado |
| Funciones edge / red edge global | No | No | No | No | No | No | Sí |
| Tarifas por asiento | No | No | No | No | $20/usuario (Pro) | Por usuario | $20/asiento (Pro) |

**Dónde ganan las alternativas.** Coolify y Dokploy tienen soporte de primera clase para Docker Compose y bibliotecas de plantillas de un clic (más de 280 aplicaciones en Coolify) que Temps todavía no tiene, y ambos cuentan con comunidades mucho más grandes — solo Coolify supera las 56k estrellas en GitHub, mientras que Temps es el proyecto más nuevo de esta lista. Kamal es la opción más sencilla si lo único que quieres son despliegues Docker sin tiempo de inactividad gestionados desde una CLI. Vercel y el resto de plataformas gestionadas te dan una red edge global, funciones edge y absorción de DDoS que un único VPS no puede igualar — y además operan la infraestructura por ti, lo cual tiene un valor real si nunca quieres preocuparte por un servidor.

Comparativas detalladas y actualizadas regularmente: [temps.sh/compare](https://temps.sh/compare)

---

## Stack tecnológico

- **Backend:** Rust, Axum, Sea-ORM, Pingora (el motor de proxy de Cloudflare), Bollard (API de Docker)
- **Frontend:** React 19, TypeScript, Tailwind CSS, shadcn/ui
- **Base de datos:** PostgreSQL + TimescaleDB
- **Arquitectura:** más de 30 crates de workspace, arquitectura de servicios en tres capas

---

## SDKs

| Paquete | Descripción |
|---|---|
| [`@temps-sdk/node-sdk`](https://www.npmjs.com/package/@temps-sdk/node-sdk) | Cliente de la API de la plataforma + seguimiento de errores compatible con Sentry |
| [`@temps-sdk/react-analytics`](https://www.npmjs.com/package/@temps-sdk/react-analytics) | Analítica para React, reproducción de sesiones, Web Vitals, seguimiento de interacción |
| [`@temps-sdk/kv`](https://www.npmjs.com/package/@temps-sdk/kv) | Almacén clave-valor serverless |
| [`@temps-sdk/blob`](https://www.npmjs.com/package/@temps-sdk/blob) | Almacenamiento de archivos (compatible con S3) |
| [`@temps-sdk/cli`](https://www.npmjs.com/package/@temps-sdk/cli) | Interfaz de línea de comandos |

<details>
<summary><strong>Ejemplos rápidos</strong></summary>

**Analítica** -- envuelve tu aplicación React y el resto es automático:

```tsx
import { TempsAnalyticsProvider } from '@temps-sdk/react-analytics';

export default function App({ children }) {
  return <TempsAnalyticsProvider>{children}</TempsAnalyticsProvider>;
}
```

**Seguimiento de errores** -- compatible con Sentry, sustituto directo:

```typescript
import { ErrorTracking } from '@temps-sdk/node-sdk';

ErrorTracking.init({ dsn: 'https://key@your-instance.temps.dev/1' });

try {
  riskyOperation();
} catch (error) {
  ErrorTracking.captureException(error);
}
```

**Almacén KV** -- API al estilo Redis, sin configuración:

```typescript
import { kv } from '@temps-sdk/kv';

await kv.set('user:123', { name: 'Alice', plan: 'pro' }, { ex: 3600 });
const user = await kv.get('user:123');
```

**Almacenamiento blob** -- sube y sirve archivos:

```typescript
import { blob } from '@temps-sdk/blob';

const { url } = await blob.put('avatars/user-123.png', fileBuffer);
const files = await blob.list({ prefix: 'avatars/' });
```

</details>

---

## Comunidad

- [GitHub Discussions](https://github.com/gotempsh/temps/discussions) — preguntas, ideas y proyectos para compartir
- [GitHub Issues](https://github.com/gotempsh/temps/issues) — informes de errores y solicitudes de funcionalidades

Si Temps te ahorra una factura de SaaS, [una estrella](https://github.com/gotempsh/temps) ayuda a que otras personas lo encuentren.

---

## Historial de estrellas

<a href="https://www.star-history.com/#gotempsh/temps&Date">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/svg?repos=gotempsh/temps&type=Date&theme=dark" />
    <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/svg?repos=gotempsh/temps&type=Date" />
    <img alt="Gráfico del historial de estrellas" src="https://api.star-history.com/svg?repos=gotempsh/temps&type=Date" />
  </picture>
</a>

---

## Contribuir

Las contribuciones son bienvenidas. Consulta [CONTRIBUTING.md](CONTRIBUTING.md) para conocer las pautas.

```bash
git clone https://github.com/gotempsh/temps.git
cd temps
cargo build --release
```

---

## Licencia

Con doble licencia [MIT](LICENSE-MIT) o [Apache 2.0](LICENSE).

---

<div align="center">

[temps.sh](https://temps.sh) | [Documentación](https://temps.sh/docs) | [GitHub](https://github.com/gotempsh/temps)

</div>
