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
  if (deployment.is_current && envUrl) return normalizeUrl(envUrl)
  if (deployment.url) return normalizeUrl(deployment.url)
  return envUrl ? normalizeUrl(envUrl) : null
}

/** Accept both bare hostnames and absolute URLs from the API. */
function normalizeUrl(value: string): string {
  return value.startsWith('http') ? value : `https://${value}`
}
