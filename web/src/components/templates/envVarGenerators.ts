/**
 * Frontend-side default-value generators for template environment variables.
 *
 * Templates can declare `default_generator` per env var (see `temps-core/templates.yaml`).
 * The Configurator uses this to (a) auto-fill empty values once the user has typed a
 * repository name and (b) render a "Generate" button on the value field.
 */

/**
 * Resolved deployment-URL parts for a given platform install.
 *
 * Kept as a struct (rather than a single string) so non-default ports and
 * non-https schemes from `external_url` survive end-to-end — e.g. a local
 * dev install at `http://localhost:8080` should generate
 * `http://my-app.localhost:8080`, not `https://my-app.localhost`.
 */
export type DeploymentUrlBase = {
  /** `https` or `http`. */
  scheme: 'http' | 'https'
  /** Bare host (no port), e.g. `example.com`. */
  host: string
  /** Optional port — e.g. `8080`. Only set when non-standard for the scheme. */
  port?: string
}

export type GeneratorContext = {
  /** Repository slug entered by the user (e.g. `my-app`). */
  repositoryName: string
  /** Resolved deployment-URL parts (see `resolveDeploymentUrlBase`). */
  base?: DeploymentUrlBase
}

/**
 * Generates a hex string of the requested byte length using the Web Crypto API.
 * Falls back to `Math.random` only when crypto is unavailable (very old browsers).
 */
function randomHex(byteLength: number): string {
  const buf = new Uint8Array(byteLength)
  if (typeof crypto !== 'undefined' && crypto.getRandomValues) {
    crypto.getRandomValues(buf)
  } else {
    for (let i = 0; i < byteLength; i++) buf[i] = Math.floor(Math.random() * 256)
  }
  return Array.from(buf, (b) => b.toString(16).padStart(2, '0')).join('')
}

/**
 * Generates a base64-url-safe random string of the requested byte length.
 */
function randomBase64(byteLength: number): string {
  const buf = new Uint8Array(byteLength)
  if (typeof crypto !== 'undefined' && crypto.getRandomValues) {
    crypto.getRandomValues(buf)
  } else {
    for (let i = 0; i < byteLength; i++) buf[i] = Math.floor(Math.random() * 256)
  }
  let binary = ''
  for (let i = 0; i < buf.length; i++) binary += String.fromCharCode(buf[i])
  return btoa(binary).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '')
}

function isLocalHost(host: string): boolean {
  return (
    host === 'localhost' ||
    host.endsWith('.localhost') ||
    host.startsWith('127.') ||
    host.startsWith('192.168.') ||
    host.startsWith('10.') ||
    host === '::1' ||
    host === '[::1]'
  )
}

/**
 * Parses `external_url` into its scheme, hostname, and (optional) port.
 * Returns `null` for empty / malformed input.
 */
function parseExternalUrl(externalUrl?: string | null): DeploymentUrlBase | null {
  const trimmed = externalUrl?.trim()
  if (!trimmed) return null
  try {
    const url = new URL(trimmed)
    if (!url.hostname) return null
    return {
      scheme: url.protocol === 'http:' ? 'http' : 'https',
      host: url.hostname,
      // `URL.port` is empty string when the port is the default for the scheme.
      port: url.port || undefined,
    }
  } catch {
    return null
  }
}

/**
 * Resolves the deployment-URL base used by the `app_url` generator.
 *
 * Priority:
 *   1. The platform's `preview_domain` (e.g. `*.example.com` → host
 *      `example.com`) — that's the value the proxy uses to route
 *      `{slug}.{preview_domain}` to deployments. When `external_url` is
 *      also set, we inherit its **scheme** and **port** so dev installs
 *      like `external_url=http://localhost:8080` + `preview_domain=*.localho.st`
 *      generate `http://my-app.localho.st:8080` instead of dropping the port.
 *   2. The full URL parts of `external_url` (scheme, host, port) when
 *      `preview_domain` is empty — generated URLs at least reach the same
 *      origin the user is configured to use.
 *   3. The current browser location, when it's not localhost — covers
 *      self-hosted installs that haven't completed onboarding yet.
 *   4. `https://temps.sh` as a final fallback for unconfigured local dev.
 */
export function resolveDeploymentUrlBase(opts?: {
  previewDomain?: string | null
  externalUrl?: string | null
}): DeploymentUrlBase {
  const externalUrlBase = parseExternalUrl(opts?.externalUrl)
  const previewDomain = opts?.previewDomain?.trim()

  if (previewDomain) {
    const host = previewDomain.replace(/^\*\./, '')
    // Inherit scheme + port from external_url when set, since `preview_domain`
    // is a DNS-only concept and doesn't carry transport details. This is what
    // makes dev installs work: `*.localho.st` + `http://localhost:8080`
    // -> `http://{slug}.localho.st:8080`.
    if (externalUrlBase) {
      return {
        scheme: externalUrlBase.scheme,
        host,
        port: externalUrlBase.port,
      }
    }
    return { scheme: 'https', host }
  }

  if (externalUrlBase) return externalUrlBase

  if (typeof window !== 'undefined' && window.location?.hostname) {
    const hostname = window.location.hostname
    if (!isLocalHost(hostname)) {
      const scheme = window.location.protocol === 'http:' ? 'http' : 'https'
      const port = window.location.port || undefined
      return { scheme, host: hostname, port }
    }
  }

  return { scheme: 'https', host: 'temps.sh' }
}

/**
 * Stringifies a `DeploymentUrlBase` for display (e.g. preview text).
 */
export function formatDeploymentUrlBase(base: DeploymentUrlBase): string {
  const portPart = base.port ? `:${base.port}` : ''
  return `${base.scheme}://${base.host}${portPart}`
}

/**
 * Computes the deployment URL for a given repository slug. Returns `null` if
 * the repo name is empty (the URL would be invalid until the user types one).
 *
 * Format: `{scheme}://{slug}.{host}[:port]` — port preserved verbatim from
 * `external_url` so non-default ports (8080, 9000, …) survive the round-trip.
 */
export function generateAppUrl(ctx: GeneratorContext): string | null {
  const slug = ctx.repositoryName?.trim()
  if (!slug) return null
  const base = ctx.base || resolveDeploymentUrlBase()
  const portPart = base.port ? `:${base.port}` : ''
  return `${base.scheme}://${slug}.${base.host}${portPart}`
}

/**
 * Runs the named generator. Returns `null` when the generator is unknown or
 * cannot produce a value yet (e.g. `app_url` with no repo name).
 */
export function runGenerator(
  generator: string | null | undefined,
  ctx: GeneratorContext
): string | null {
  if (!generator) return null
  switch (generator) {
    case 'app_url':
      return generateAppUrl(ctx)
    case 'random_hex_32':
      return randomHex(32)
    case 'random_secret':
      return randomBase64(32)
    default:
      return null
  }
}

/**
 * Whether the named generator produces a value that depends on the repository
 * name. Used to decide whether to re-run the generator when the repo or the
 * resolved deployment-URL base changes.
 */
export function generatorDependsOnRepoName(generator: string | null | undefined): boolean {
  return generator === 'app_url'
}
