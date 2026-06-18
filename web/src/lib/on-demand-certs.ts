/**
 * Hand-written helpers for the on-demand TLS certificate endpoints (ADR-018 §5)
 * that are not yet reflected in the generated OpenAPI client.
 *
 * These reuse the generated `client` transport (baseUrl `/api`, bearer auth,
 * error parsing) rather than hand-rolling `fetch`, so behaviour matches the rest
 * of the SDK. Once `bun run openapi-ts` is re-run against a server that exposes
 * these endpoints, this file can be deleted and the generated SDK used directly.
 *
 * TODO(sdk-regen): replace with generated SDK helpers for
 *   - GET /domains/on-demand-certs
 *   - GET /domains/by-host/{hostname}/cert-status
 * once these endpoints are included in the OpenAPI spec / generated client.
 */

import { queryOptions } from '@tanstack/react-query'
import { client } from '@/api/client/client.gen'

/** Mirror of the backend `OnDemandCertAttemptResponse` DTO. */
export interface OnDemandCertAttempt {
  id: number
  hostname: string
  trigger: string
  challenge_served: boolean | null
  acme_request_sent: boolean | null
  acme_response_status: string | null
  outcome: string
  error_chain: string | null
  error_category: string | null
  duration_ms: number | null
  created_at: number
}

/** Mirror of the backend `OnDemandCertRow` DTO. */
export interface OnDemandCertRow {
  hostname: string
  status: string | null
  expiration_time: number | null
  backoff_until: number | null
  attempt: OnDemandCertAttempt
}

/** Mirror of the backend `ListOnDemandCertsResponse` DTO. */
export interface ListOnDemandCertsResponse {
  certs: OnDemandCertRow[]
  total: number
  page: number
  page_size: number
}

/** Mirror of the backend `CertStatusResponse` DTO. */
export interface CertStatusResponse {
  hostname: string
  status: string | null
  backoff_until: number | null
  last_attempt: OnDemandCertAttempt | null
}

export interface ListOnDemandCertsQuery {
  page?: number
  page_size?: number
}

const BEARER_SECURITY = [{ scheme: 'bearer', type: 'http' }] as const

export async function listOnDemandCerts(
  query: ListOnDemandCertsQuery,
  signal?: AbortSignal,
): Promise<ListOnDemandCertsResponse> {
  const { data } = await client.get<ListOnDemandCertsResponse, unknown, true>({
    security: [...BEARER_SECURITY],
    url: '/domains/on-demand-certs',
    query: query as Record<string, unknown>,
    signal,
    throwOnError: true,
  })
  return data
}

export async function getOnDemandCertStatus(
  hostname: string,
  signal?: AbortSignal,
): Promise<CertStatusResponse> {
  const { data } = await client.get<CertStatusResponse, unknown, true>({
    security: [...BEARER_SECURITY],
    url: '/domains/by-host/{hostname}/cert-status',
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
  backoffUntil?: number | null,
): string | null {
  if (!category) return null
  const retry =
    typeof backoffUntil === 'number'
      ? ` — retry after ${new Date(backoffUntil).toLocaleString()}`
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
