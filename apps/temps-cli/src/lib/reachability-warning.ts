// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * The reachability warning a finished deploy carries (ADR 068 D5), as
 * `temps deploy` prints it. The words match the block the server writes at
 * the end of the deploy log, so the terminal and the console say the same.
 */

export interface ReachabilityFix {
  kind: string
  label: string
  href?: string | null
  command?: string | null
}

export interface ReachabilityWarning {
  environment_id: number
  environment: string
  headline: string
  reason: string
  fixes: ReachabilityFix[]
}

function lowerFirst(text: string): string {
  return text.length > 0 ? text[0]!.toLowerCase() + text.slice(1) : text
}

export function reachabilityWarningLines(warning: ReachabilityWarning): string[] {
  const lines = [`⚠ ${warning.headline}:`, `  ${warning.reason}`]
  if (warning.fixes.length === 0) return lines
  lines.push('  Make it reachable:')
  warning.fixes.forEach((fix, index) => {
    const label = lowerFirst(
      fix.command ? `${fix.label}: ${fix.command}` : fix.label,
    )
    lines.push(index === 0 ? `    • ${label}` : `    • or ${label}`)
  })
  return lines
}

/**
 * The environment's link: the primary address the server names, then the
 * first domain (which the server already orders primary first). `null` when
 * the server knows nobody can reach it.
 */
export function primaryEnvironmentUrl(environment: {
  domains?: string[]
  primary_url?: string | null
  no_address_reason?: string | null
}): string | null {
  if (environment.no_address_reason) return null
  const candidate = environment.primary_url ?? environment.domains?.[0]
  if (!candidate) return null
  return /^https?:\/\//i.test(candidate) ? candidate : `https://${candidate}`
}
