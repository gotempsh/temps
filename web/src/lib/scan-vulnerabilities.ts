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

/** The server caps a page of findings here; asking for more silently returns this many. */
export const SCAN_VULNERABILITIES_PAGE_SIZE = 100

/** Upper bound on pages walked, so a malformed `total` cannot loop forever. */
export const MAX_VULNERABILITY_PAGES = 200

/**
 * The walk gave up before reaching the end of the scan's findings. This is
 * not "the finding does not exist": the answer is unknown.
 */
export class VulnerabilitySearchIncompleteError extends Error {
  readonly pagesSearched: number
  constructor(vulnId: string, pagesSearched: number) {
    super(
      `Vulnerability ${vulnId} was not found in the first ${pagesSearched} pages of this scan's findings, and the list is longer. Try again, or open it from the scan's list.`
    )
    this.name = 'VulnerabilitySearchIncompleteError'
    this.pagesSearched = pagesSearched
  }
}

function envelopeTotal(response: unknown): number | undefined {
  if (response && typeof response === 'object' && 'total' in response) {
    const { total } = response as { total: unknown }
    if (typeof total === 'number' && Number.isFinite(total)) return total
  }
  return undefined
}

/**
 * Find one finding by walking a scan's pages until it turns up or the list
 * ends. A scan can have far more findings than one page holds, so searching
 * only the first page reports a real finding as "not found".
 *
 * Resolves `undefined` only when the end of the list was reached without the
 * finding (or the id is not a number, which fetches nothing). If the walk is
 * stopped by `maxPages` it rejects with `VulnerabilitySearchIncompleteError`
 * instead of claiming the finding is absent.
 *
 * The end is decided from the findings actually received, not from the
 * requested page size: a server that returns shorter pages than asked for
 * must not end the walk early. `signal` stops the walk before the next page
 * is requested.
 */
export async function findVulnerabilityInPages({
  vulnId,
  fetchPage,
  signal,
  pageSize = SCAN_VULNERABILITIES_PAGE_SIZE,
  maxPages = MAX_VULNERABILITY_PAGES,
}: {
  vulnId: string | undefined
  /** `page` is 1-based; `signal` is the walk's cancellation signal. */
  fetchPage: (page: number, signal?: AbortSignal) => Promise<unknown>
  signal?: AbortSignal
  pageSize?: number
  maxPages?: number
}): Promise<VulnerabilityResponse | undefined> {
  if (!vulnId || !/^\d+$/.test(vulnId)) return undefined
  let received = 0
  for (let page = 1; page <= maxPages; page++) {
    signal?.throwIfAborted()
    const response = await fetchPage(page, signal)
    const rows = scanVulnerabilities(response)
    const found = findScanVulnerability(rows, vulnId)
    if (found) return found
    received += rows.length
    const total = envelopeTotal(response)
    // Without a total, a page shorter than requested is the only end signal.
    const reachedEnd =
      rows.length === 0 ||
      (total !== undefined ? received >= total : rows.length < pageSize)
    if (reachedEnd) return undefined
  }
  throw new VulnerabilitySearchIncompleteError(vulnId, maxPages)
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
