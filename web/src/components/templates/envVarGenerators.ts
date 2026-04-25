/**
 * Frontend-side default-value generators for template environment variables.
 *
 * Templates can declare `default_generator` per env var (see `temps-core/templates.yaml`).
 * The Configurator uses this to (a) auto-fill empty values once the user has typed a
 * repository name and (b) render a "Generate" button on the value field.
 */

export type GeneratorContext = {
  /** Repository slug entered by the user (e.g. `my-app`). */
  repositoryName: string
  /**
   * Base domain used for derived URLs. Falls back to `temps.sh`.
   * Sourced from `window.location.host` when running against the live console.
   */
  baseDomain?: string
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

/**
 * Returns the inferred base domain — either the host the user is currently on
 * (so self-hosted installs derive their own URL) or `temps.sh` as a default.
 */
export function inferBaseDomain(): string {
  if (typeof window !== 'undefined' && window.location?.hostname) {
    const host = window.location.host
    // For local dev, prefer the canonical placeholder over `localhost:8080`.
    if (host.includes('localhost') || host.startsWith('127.') || host.includes(':')) {
      return 'temps.sh'
    }
    return host
  }
  return 'temps.sh'
}

/**
 * Computes the deployment URL for a given repository slug. Returns `null` if
 * the repo name is empty (the URL would be invalid until the user types one).
 */
export function generateAppUrl(ctx: GeneratorContext): string | null {
  const slug = ctx.repositoryName?.trim()
  if (!slug) return null
  const domain = ctx.baseDomain || inferBaseDomain()
  return `https://${slug}.${domain}`
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
 * name. Used to decide whether to re-run the generator when the repo changes.
 */
export function generatorDependsOnRepoName(generator: string | null | undefined): boolean {
  return generator === 'app_url'
}
