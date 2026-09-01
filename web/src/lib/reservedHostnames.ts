// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Hostnames the platform serves itself and that must never be routed at a
 * project (issue #478).
 *
 * Assigning the console hostname to a project hijacks the proxy route for the
 * control plane, and the only way back in is the server's public IP. The API
 * rejects these too — this mirrors the rule client-side so the user sees it
 * before submitting.
 */

function normalizeHost(value: string | null | undefined): string | null {
  if (!value) return null
  const raw = value.trim()
  if (!raw) return null
  try {
    const url = new URL(raw.includes('://') ? raw : `https://${raw}`)
    return url.hostname.replace(/\.$/, '').toLowerCase()
  } catch {
    return null
  }
}

/** Hostname the Temps console is served on, derived from `external_url`. */
export function consoleHostname(
  settings: { external_url?: string | null } | undefined | null
): string | null {
  return normalizeHost(settings?.external_url)
}

/**
 * Returns the reason `domain` is reserved, or `null` when it is assignable.
 * Mirrors `AppSettings::is_reserved_hostname` on the server.
 */
export function reservedHostnameReason(
  domain: string,
  settings:
    | { external_url?: string | null; preview_domain?: string | null }
    | undefined
    | null
): string | null {
  const host = domain.trim().replace(/\.$/, '').toLowerCase()
  if (!host || host.includes('*')) return null

  if (consoleHostname(settings) === host) {
    return 'This is the Temps console domain. Routing it to a project would make the console unreachable.'
  }

  const preview = settings?.preview_domain
    ?.trim()
    .replace(/\.$/, '')
    .toLowerCase()
  if (preview && preview === host) {
    return 'This is the preview domain Temps generates deployment URLs from and cannot be assigned to a project.'
  }

  return null
}
