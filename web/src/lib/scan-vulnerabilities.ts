// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import type { VulnerabilityResponse } from '@/api/client/types.gen'
import { isVerifiedNotFound } from '@/lib/read-failure'

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

/**
 * Locate one vulnerability in a scan's list from the raw route parameter.
 *
 * Returns undefined for a missing, non-numeric or fractional id so a mangled
 * URL reads as "not found" instead of matching id `0` or throwing.
 */
export function findScanVulnerability(
  vulnerabilities: readonly VulnerabilityResponse[],
  vulnId: string | undefined
): VulnerabilityResponse | undefined {
  if (!vulnId || !/^\d+$/.test(vulnId)) return undefined
  const id = Number(vulnId)
  return vulnerabilities.find((vulnerability) => vulnerability.id === id)
}

/**
 * What the vulnerability detail page renders.
 *
 * - `not-found`: the scan is verifiably gone (404), the list read answered
 *   404, or the list settled without the requested id. An unknown scan id
 *   answers the list endpoint with an empty envelope, so both end here.
 * - `failed`: the list read failed for another reason (5xx, 403, network).
 *   Nothing is known about the vulnerability, so the page offers a retry.
 * - `loading`: the list read is in flight.
 * - `ready`: the vulnerability was found. A failed scan-context read does not
 *   demote this: the scan summary is optional on the detail page.
 */
export type VulnerabilityDetailState =
  'loading' | 'ready' | 'not-found' | 'failed'

export function vulnerabilityDetailState({
  scanError,
  vulnerabilitiesError,
  isLoading,
  vulnerability,
}: {
  scanError: unknown
  vulnerabilitiesError: unknown
  isLoading: boolean
  vulnerability: VulnerabilityResponse | undefined
}): VulnerabilityDetailState {
  if (isVerifiedNotFound(scanError)) return 'not-found'
  if (vulnerabilitiesError) {
    return isVerifiedNotFound(vulnerabilitiesError) ? 'not-found' : 'failed'
  }
  if (isLoading) return 'loading'
  return vulnerability ? 'ready' : 'not-found'
}
