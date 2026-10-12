// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Domain status helpers — keep in sync with the backend's
// `temps_entities::domains` status constants (CERT_SERVING_STATUSES).

/** Domain has a certificate and is serving normally. */
export const STATUS_ACTIVE = 'active'

/**
 * Domain still has a valid certificate that is being served, but its last
 * renewal attempt failed. Degraded-but-serving: surface a warning so the
 * operator can fix the renewal before the existing certificate expires.
 */
export const STATUS_ACTIVE_RENEWAL_FAILED = 'active_renewal_failed'

/**
 * Statuses for which the domain is serving a live certificate. The proxy
 * serves certs for exactly these statuses, so the UI should treat all of them
 * as "active" for the purposes of showing cert details, renew actions, and
 * expiry warnings.
 */
export const CERT_SERVING_STATUSES: readonly string[] = [
  STATUS_ACTIVE,
  STATUS_ACTIVE_RENEWAL_FAILED,
]

/** True when the domain is serving a live certificate (active or renewal-failed). */
export function isServingCert(status: string | null | undefined): boolean {
  return !!status && CERT_SERVING_STATUSES.includes(status)
}

/**
 * Statuses a domain passes through on its way to a certificate: the order is
 * being set up, waiting for the challenge to be published, or being validated
 * and issued. Anything else (`active`, `active_renewal_failed`, `failed`,
 * `expired`, ...) is settled until the operator acts again.
 */
export const ISSUANCE_IN_FLIGHT_STATUSES: readonly string[] = [
  'pending',
  'pending_dns',
  'pending_http',
  'challenge_requested',
  'processing',
  'on_demand_issuing',
]

/** True while issuance may still move the domain to a different status. */
export function isIssuanceInFlight(status: string | null | undefined): boolean {
  return !!status && ISSUANCE_IN_FLIGHT_STATUSES.includes(status)
}

/** How often to re-read a domain that is mid-issuance. */
export const DOMAIN_POLL_FAST_MS = 3_000
/** Back-off once a status has stayed in flight for a minute or so. */
export const DOMAIN_POLL_SLOW_MS = 15_000
/** Number of fast reads before backing off (~1 minute at the fast rate). */
export const DOMAIN_POLL_FAST_READS = 20

/**
 * React Query `refetchInterval` for a domain read: poll while issuance is in
 * flight, and stop (`false`) as soon as the status is settled so a failed
 * attempt shows up without a reload and a finished one stops costing requests.
 * `readCount` is how many successful reads the query has already made.
 */
export function domainRefetchInterval(
  status: string | null | undefined,
  readCount: number
): number | false {
  if (!isIssuanceInFlight(status)) return false
  return readCount < DOMAIN_POLL_FAST_READS
    ? DOMAIN_POLL_FAST_MS
    : DOMAIN_POLL_SLOW_MS
}
