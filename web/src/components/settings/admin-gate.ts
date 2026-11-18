// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** Saved admin gate rules, as returned by `GET /admin/gate-settings`. */
interface AdminGateRules {
  allowed_ips?: string[] | null
  allowed_hosts?: string[] | null
}

/**
 * True when the saved rules restrict nothing: the console then answers on
 * every hostname and address, including the server's bare IP.
 */
export function adminGateIsOpen(rules: AdminGateRules): boolean {
  return (
    (rules.allowed_ips ?? []).length === 0 &&
    (rules.allowed_hosts ?? []).length === 0
  )
}

/**
 * The hostname to offer as the console's only allowed `Host`, taken from the
 * address the admin is using right now (so saving it cannot lock them out).
 * `null` when that address is an IP literal or loopback name: restricting
 * the console to it would not hide it from anyone.
 */
export function suggestedConsoleHost(hostname: string): string | null {
  const host = hostname.trim().toLowerCase()
  if (!host || host === 'localhost' || host.endsWith('.localhost')) return null
  if (host.includes(':') || host.startsWith('[')) return null // IPv6 literal
  if (/^\d{1,3}(\.\d{1,3}){3}$/.test(host)) return null // IPv4 literal
  return host
}
