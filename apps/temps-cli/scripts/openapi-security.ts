// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Every operation in the committed spec must say how it is authenticated.
 *
 * The spec has no global `security`, so an operation that does not declare
 * `security(("bearer_auth" = []))` reads as public, in the document and in
 * every client generated from it, even when its handler requires a token.
 * That is how the environment, variable and secret endpoints came to look
 * unauthenticated.
 *
 * `spec:check` fails when an operation without a `security` declaration is
 * not in `openapi-unsecured-operations.json`. That file lists the operations
 * that had none when this check was added: endpoints that are public by
 * design (login, public status pages, browser ingest) and authenticated
 * endpoints that predate the check alike. It may only shrink: an entry whose
 * operation now declares security, or no longer exists, fails the check too,
 * so fixing one removes it. A new authenticated route declares bearer auth;
 * a new public one is added to the list in the same review.
 */

export const BASELINE_PATH = new URL('../openapi-unsecured-operations.json', import.meta.url).pathname

const METHODS = ['get', 'put', 'post', 'delete', 'options', 'head', 'patch', 'trace'] as const

/**
 * Whether a `security` list requires authentication. The entries are
 * alternatives, and an empty requirement object (`{}`) is the anonymous
 * alternative, so a list that contains one, even next to `bearer_auth`,
 * lets an unauthenticated caller in.
 */
export function requiresAuthentication(security: unknown): boolean {
  return (
    Array.isArray(security) &&
    security.length > 0 &&
    security.every(
      (requirement) =>
        requirement !== null && typeof requirement === 'object' && Object.keys(requirement).length > 0,
    )
  )
}

/** `METHOD /path` of every operation whose `security` does not require authentication, sorted. */
export function operationsWithoutSecurity(spec: unknown): string[] {
  const paths = (spec as { paths?: Record<string, Record<string, unknown>> } | null)?.paths ?? {}
  const unsecured: string[] = []
  for (const [path, item] of Object.entries(paths)) {
    for (const method of METHODS) {
      const operation = item?.[method] as { security?: unknown } | undefined
      if (!operation) continue
      if (!requiresAuthentication(operation.security)) {
        unsecured.push(`${method.toUpperCase()} ${path}`)
      }
    }
  }
  return unsecured.sort()
}

export interface SecurityDrift {
  /** Operations without security that the baseline does not list. */
  undeclared: string[]
  /** Baseline entries whose operation now declares security or is gone. */
  stale: string[]
}

/** Compare the spec's unsecured operations with the baseline. */
export function securityDrift(spec: unknown, baseline: readonly string[]): SecurityDrift {
  const unsecured = operationsWithoutSecurity(spec)
  const listed = new Set(baseline)
  const current = new Set(unsecured)
  return {
    undeclared: unsecured.filter((operation) => !listed.has(operation)),
    stale: [...listed].filter((operation) => !current.has(operation)).sort(),
  }
}

/** Human-readable failure for `spec:check`; `null` when nothing drifted. */
export function describeSecurityDrift(drift: SecurityDrift): string | null {
  const lines: string[] = []
  if (drift.undeclared.length) {
    lines.push(
      'These operations declare no security, so the spec and the generated clients present them as public:',
      ...drift.undeclared.map((operation) => `  ${operation}`),
      'If the handler requires authentication, add security(("bearer_auth" = [])) to its #[utoipa::path].',
      'If it is public by design, add it to apps/temps-cli/openapi-unsecured-operations.json.',
    )
  }
  if (drift.stale.length) {
    if (lines.length) lines.push('')
    lines.push(
      'These entries in apps/temps-cli/openapi-unsecured-operations.json now declare security or no longer exist; remove them:',
      ...drift.stale.map((operation) => `  ${operation}`),
    )
  }
  return lines.length ? lines.join('\n') : null
}
