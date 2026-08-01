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
 * Resolve the URL for a *project-level* "Visit" affordance.
 *
 * Unlike {@link resolvePrimaryUrl}, this never falls back to a deployment's own
 * slug-derived host. The project header is about the project's live site, not
 * about one build — and the newest deployment is not necessarily the current
 * one: `get_last_deployment` orders by `created_at` with no status filter, so
 * while a deploy is running, or after one fails, `is_current` is false. Falling
 * back to that build's ephemeral host would hand the user a link that was never
 * routed, in exactly the situation this helper exists to avoid.
 */
export function resolveStableUrl(
  deployment: DeploymentResponse
): string | null {
  const envUrl = deployment.environment.domains?.[0]
  return envUrl ? normalizeUrl(envUrl) : null
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
export function normalizeUrl(value: string): string | null {
  // The `//` is required: without it a bare `app.example.com:8080` is read as
  // scheme `app.example.com:` and rejected, dropping a legitimate host:port.
  const hasScheme = /^[a-z][a-z0-9+.-]*:\/\//i.test(value)
  // A schemeless value must be a bare hostname. Leading `/` or `\` is
  // protocol-relative or a path, and for special schemes the URL parser treats
  // `\` as `/` and ignores any number of them — so `//evil.com`, `\\evil.com`
  // and `/\evil.com` would all resolve to origin `evil.com` once prefixed.
  if (!hasScheme && /^[/\\]/.test(value)) return null
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
