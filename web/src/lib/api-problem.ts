// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Utilities for extracting human-readable information from RFC 7807
 * Problem Details error objects.
 *
 * These helpers are domain-agnostic and can be used alongside any generated
 * SDK endpoint.
 */

/**
 * Pull the human-readable sentence out of an RFC 7807 Problem body.
 *
 * The gate's refusals carry the *specific* missing prerequisite in `detail`,
 * and that sentence is the entire value of the error — collapsing it to
 * "Request failed" would leave a self-hosted operator with nothing to act on.
 */
export function problemDetail(error: unknown, fallback: string): string {
  if (error && typeof error === 'object') {
    const problem = error as { detail?: unknown; title?: unknown }
    if (typeof problem.detail === 'string' && problem.detail.length > 0) {
      return problem.detail
    }
    if (typeof problem.title === 'string' && problem.title.length > 0) {
      return problem.title
    }
  }
  return fallback
}

/** The console path an RFC 7807 gate refusal points at, when it carries one. */
export function problemSetupPath(error: unknown): string | undefined {
  if (error && typeof error === 'object') {
    const problem = error as { setup_path?: unknown }
    if (typeof problem.setup_path === 'string') return problem.setup_path
  }
  return undefined
}
