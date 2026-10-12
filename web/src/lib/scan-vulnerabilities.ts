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
 * Why a search ended without an answer:
 *
 * - `too-large`: the walk read `maxPages` pages and the list goes on. The
 *   scan holds more findings than this page will read, so searching again
 *   gives the same answer.
 * - `unstable-order`: the list ended without the finding, but the pages did
 *   not add up (a finding was served twice or the unique count fell short of
 *   the reported total). The server orders findings without a unique
 *   tiebreaker, so rows that tie can be dealt differently to each page
 *   request and one can fall between two pages. The finding may exist, and a
 *   fresh walk can land differently.
 */
export type VulnerabilitySearchIncompleteReason = 'too-large' | 'unstable-order'

/**
 * The walk ended without proving the finding is absent. This is not "the
 * finding does not exist": the answer is unknown.
 */
export class VulnerabilitySearchIncompleteError extends Error {
  readonly reason: VulnerabilitySearchIncompleteReason
  readonly pagesSearched: number
  /** Whether a second walk can plausibly answer differently. */
  readonly retryable: boolean
  constructor({
    reason,
    vulnId,
    pagesSearched,
    pageSize,
    total,
  }: {
    reason: VulnerabilitySearchIncompleteReason
    vulnId: string
    pagesSearched: number
    pageSize: number
    total?: number
  }) {
    const searched = pagesSearched * pageSize
    super(
      reason === 'too-large'
        ? `Vulnerability ${vulnId} is not among the first ${searched} findings of this scan${
            total !== undefined ? `, which has ${total}` : ''
          }. The console cannot search further than that, so this finding may still exist.`
        : `Vulnerability ${vulnId} was not found, but the server returned this scan's findings in a different order between pages, so some may have been skipped. Search again to check.`
    )
    this.name = 'VulnerabilitySearchIncompleteError'
    this.reason = reason
    this.pagesSearched = pagesSearched
    this.retryable = reason === 'unstable-order'
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
 * finding, every row the server counted was seen exactly once (or the id is
 * not a number, which fetches nothing). If the walk is stopped by `maxPages`,
 * or the pages do not add up, it rejects with
 * `VulnerabilitySearchIncompleteError` instead of claiming the finding is
 * absent.
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
  let total: number | undefined
  // At most `maxPages * pageSize` ids, so memory is bounded by the page cap.
  const seen = new Set<number>()
  for (let page = 1; page <= maxPages; page++) {
    signal?.throwIfAborted()
    const response = await fetchPage(page, signal)
    const rows = scanVulnerabilities(response)
    const found = findScanVulnerability(rows, vulnId)
    if (found) return found
    received += rows.length
    for (const row of rows) seen.add(row.id)
    total = envelopeTotal(response)
    // Without a total, a page shorter than requested is the only end signal.
    const reachedEnd =
      rows.length === 0 ||
      (total !== undefined ? received >= total : rows.length < pageSize)
    if (reachedEnd) {
      // A complete walk reads every finding once. A repeat, or fewer distinct
      // findings than the server counted, means a tied row fell between two
      // pages, so "not found" would not be proven.
      const pagesDidNotAddUp =
        seen.size < received || (total !== undefined && seen.size < total)
      if (pagesDidNotAddUp) {
        throw new VulnerabilitySearchIncompleteError({
          reason: 'unstable-order',
          vulnId,
          pagesSearched: page,
          pageSize,
          total,
        })
      }
      return undefined
    }
  }
  throw new VulnerabilitySearchIncompleteError({
    reason: 'too-large',
    vulnId,
    pagesSearched: maxPages,
    pageSize,
    total,
  })
}
