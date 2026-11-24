// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** The problem title the API gives when the sandbox's host cannot share the service-access socket. */
export const SERVICE_ACCESS_UNAVAILABLE_TITLE =
  'Service access is unavailable for this sandbox'

/** True for the refusal that no retry can fix: this host, or this sandbox's image, cannot host service access. */
export function isServiceAccessUnavailable(error: unknown): boolean {
  return (
    !!error &&
    typeof error === 'object' &&
    (error as { title?: unknown }).title === SERVICE_ACCESS_UNAVAILABLE_TITLE
  )
}
