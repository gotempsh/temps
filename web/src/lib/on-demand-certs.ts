// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Helpers for the on-demand TLS certificate endpoints (ADR-018 §5):
 * `GET /domains/on-demand-certs` and `GET /domains/by-host/{hostname}/cert-status`.
 * The requests and response types are the generated SDK's; this module adds
 * the query options the certificates page uses and the error labels.
 */

import { queryOptions } from '@tanstack/react-query'
import {
  getOnDemandCertStatus as sdkGetOnDemandCertStatus,
  listOnDemandCerts as sdkListOnDemandCerts,
} from '@/api/client/sdk.gen'
import type {
  CertStatusResponse,
  ListOnDemandCertsResponse,
  OnDemandCertAttemptResponse,
  OnDemandCertRow,
} from '@/api/client/types.gen'

// The response shapes are the generated ones, so a server-side change (such
// as the timestamps becoming ISO 8601 strings) reaches this module's callers
// as a type error instead of a silent mismatch.
export type OnDemandCertAttempt = OnDemandCertAttemptResponse
export type { CertStatusResponse, ListOnDemandCertsResponse, OnDemandCertRow }

export interface ListOnDemandCertsQuery {
  page?: number
  page_size?: number
}

export async function listOnDemandCerts(
  query: ListOnDemandCertsQuery,
  signal?: AbortSignal
): Promise<ListOnDemandCertsResponse> {
  const { data } = await sdkListOnDemandCerts({
    query,
    signal,
    throwOnError: true,
  })
  return data
}

export async function getOnDemandCertStatus(
  hostname: string,
  signal?: AbortSignal
): Promise<CertStatusResponse> {
  const { data } = await sdkGetOnDemandCertStatus({
    path: { hostname },
    signal,
    throwOnError: true,
  })
  return data
}

export function listOnDemandCertsOptions(query: ListOnDemandCertsQuery) {
  return queryOptions({
    queryKey: ['listOnDemandCerts', query],
    queryFn: ({ signal }) => listOnDemandCerts(query, signal),
  })
}

export function getOnDemandCertStatusOptions(hostname: string | undefined) {
  return queryOptions({
    queryKey: ['getOnDemandCertStatus', hostname],
    queryFn: ({ signal }) => getOnDemandCertStatus(hostname as string, signal),
    enabled: !!hostname,
  })
}

/**
 * Human-readable label for an `error_category` value (ADR-018 §5). Combines the
 * coarse category with the ACME response status / backoff window where it adds
 * signal, so the operator gets an actionable message rather than a raw code.
 */
export function errorCategoryLabel(
  category: string | null | undefined,
  acmeResponseStatus?: string | null,
  backoffUntil?: string | number | null
): string | null {
  if (!category) return null
  // The API sends the deadline as ISO 8601; an unreadable one adds nothing.
  const deadline =
    typeof backoffUntil === 'string'
      ? Date.parse(backoffUntil)
      : (backoffUntil ?? Number.NaN)
  const retry = Number.isFinite(deadline)
    ? ` — retry after ${new Date(deadline).toLocaleString()}`
    : ''
  switch (category) {
    case 'rate_limited':
      return `Rate limit from Let's Encrypt${retry}`
    case 'dns_failure':
      return 'DNS lookup failed for hostname — does it resolve to this server?'
    case 'challenge_mismatch':
      return 'HTTP-01 challenge not served — is port 80 open and reachable?'
    case 'acme_order_expired':
      return 'ACME order expired before validation completed'
    case 'timeout':
      return 'Issuance timed out before completing'
    case 'internal':
      return acmeResponseStatus
        ? `Internal issuance error (${acmeResponseStatus})`
        : 'Internal issuance error'
    default:
      return category
  }
}
