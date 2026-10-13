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
 * ACME order states (RFC 8555) that can never lead to a certificate: the order
 * was cancelled, or the CA invalidated/revoked/deactivated it. Nothing can be
 * re-submitted on such an order; a new one has to be created.
 */
export const TERMINAL_ORDER_STATUSES: readonly string[] = [
  'cancelled',
  'canceled',
  'invalid',
  'revoked',
  'deactivated',
]

/** True when an ACME order in `status` can no longer be finalized. */
export function isTerminalOrderStatus(
  status: string | null | undefined
): boolean {
  return !!status && TERMINAL_ORDER_STATUSES.includes(status)
}

/**
 * What "Retry" means for a failed domain.
 *
 * - `verify`: the ACME challenge is still open (e.g. the lookup timed out), so
 *   asking the CA to check it again is meaningful.
 * - `new-order`: there is no order, or the CA already invalidated it. Verifying
 *   again would only resubmit a dead challenge ("Cannot update challenge with
 *   status invalid"), so the retry must create a new order with new DNS/HTTP
 *   challenge values.
 */
export type FailedRetryAction = 'verify' | 'new-order'

export function failedRetryAction(
  order: { status: string } | null | undefined
): FailedRetryAction {
  return !order || isTerminalOrderStatus(order.status) ? 'new-order' : 'verify'
}
