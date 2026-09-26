// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import type { VulnerabilityResponse } from '@/api/client/types.gen'

/** The deployed API returns a pagination envelope; older clients advertise an array. */
export function scanVulnerabilities(
  response: unknown
): VulnerabilityResponse[] {
  const rows = Array.isArray(response)
    ? response
    : response && typeof response === 'object' && 'data' in response
      ? response.data
      : undefined
  if (
    !Array.isArray(rows) ||
    !rows.every(
      (row) =>
        row &&
        typeof row === 'object' &&
        typeof row.vulnerability_id === 'string' &&
        typeof row.package_name === 'string' &&
        typeof row.severity === 'string'
    )
  ) {
    throw new Error(
      'The scan returned an invalid vulnerability list. Try again or check that the console and server versions match.'
    )
  }
  return rows as VulnerabilityResponse[]
}
