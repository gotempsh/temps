import type { DeploymentResponse } from '@/api/client'

/**
 * Resolve the URL a user should be sent to when they click "Visit" for a
 * deployment.
 *
 * A deployment's own `url` is derived from its slug (`{project}-{n}`), so it is
 * ephemeral: it changes on every deploy and points at that specific build. The
 * environment's stable URL comes through `environment.domains` (`domains[0]` is
 * the env URL, followed by any active custom domains).
 *
 * The current deployment is the one actually served at the environment's stable
 * domain, so surface that; older deployments have no stable domain of their own
 * and fall back to their deployment-specific URL.
 */
export function resolvePrimaryUrl(
  deployment: DeploymentResponse
): string | null {
  const envUrl = deployment.environment.domains?.[0]
  // Ordered candidates; the first that resolves to a usable http(s) URL wins.
  // A rejected candidate falls through rather than yielding null, so one bad
  // stored domain cannot suppress an otherwise valid link.
  const candidates = deployment.is_current
    ? [envUrl, deployment.url]
    : [deployment.url, envUrl]

  for (const candidate of candidates) {
    if (!candidate) continue
    const url = normalizeUrl(candidate)
    if (url) return url
  }
  return null
}

/**
 * Accept both bare hostnames and absolute URLs from the API, and resolve to an
 * `http:`/`https:` URL or nothing.
 *
 * The result is rendered as a user-followable link, so the scheme is checked by
 * parsing rather than by prefix. A `startsWith('http')` test would pass
 * `httpfoo://evil.com` through verbatim, and would prepend `https://` to a
 * protocol-relative `//evil.com` — which the URL parser then reads as origin
 * `evil.com`. Environment domains are partly user-supplied (custom domains), so
 * this must not rely on the server having sanitized them.
 */
function normalizeUrl(value: string): string | null {
  const hasScheme = /^[a-z][a-z0-9+.-]*:/i.test(value)
  // A schemeless value must be a bare hostname. Anything starting with `/` is
  // protocol-relative or a path: prepending `https://` to `//evil.com` yields
  // `https:////evil.com`, which parses with an `https:` protocol but which
  // browsers resolve to origin `evil.com`.
  if (!hasScheme && value.startsWith('/')) return null
  const candidate = hasScheme ? value : `https://${value}`
  try {
    const { protocol } = new URL(candidate)
    // Parse to validate, but return the candidate verbatim — `URL.toString()`
    // re-serializes (appending a trailing slash to a bare origin), which would
    // change the URL we display and link to.
    return protocol === 'http:' || protocol === 'https:' ? candidate : null
  } catch {
    return null
  }
}
