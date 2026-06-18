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
