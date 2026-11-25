// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Where a console address can be reached from, judged from the address
 * alone. It decides what the console tells the user ("your agent must run on
 * this computer"), never what is allowed: the Claude app extension enforces
 * its own transport rule on the addresses it actually connects to.
 */
export type ConsoleReach = 'loopback' | 'private' | 'public'

/** The four octets of a dotted IPv4 literal, or null. */
export function parseIpv4(host: string): number[] | null {
  const match = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/.exec(host)
  if (!match) return null
  const octets = match.slice(1).map(Number)
  return octets.every((o) => o <= 255) ? octets : null
}

/** A URL hostname without the brackets URL puts around an IPv6 literal. */
function bareHost(hostname: string): string {
  const lower = hostname.toLowerCase()
  return lower.startsWith('[') && lower.endsWith(']')
    ? lower.slice(1, -1)
    : lower
}

export function isLoopbackHost(hostname: string): boolean {
  const host = bareHost(hostname)
  const v4 = parseIpv4(host)
  return (
    host === 'localhost' ||
    host.endsWith('.localhost') ||
    host === '::1' ||
    (v4 !== null && v4[0] === 127)
  )
}

/**
 * A Tailscale address: 100.64.0.0/10, fd7a:115c:a1e0::/48, or a MagicDNS
 * name under ts.net. Traffic to these goes through WireGuard.
 */
export function isTailscaleHost(hostname: string): boolean {
  const host = bareHost(hostname)
  const v4 = parseIpv4(host)
  if (v4) return v4[0] === 100 && v4[1] >= 64 && v4[1] <= 127
  return host.startsWith('fd7a:115c:a1e0:') || host.endsWith('.ts.net')
}

const PRIVATE_SUFFIXES = [
  '.local',
  '.lan',
  '.internal',
  '.home.arpa',
  '.ts.net',
]

/** An address only a private network resolves or routes. */
export function isPrivateHost(hostname: string): boolean {
  const host = bareHost(hostname)
  if (isTailscaleHost(host)) return true
  const v4 = parseIpv4(host)
  if (v4) {
    const [a, b] = v4
    return (
      a === 10 ||
      (a === 172 && b >= 16 && b <= 31) ||
      (a === 192 && b === 168) ||
      (a === 169 && b === 254)
    )
  }
  if (host.includes(':')) {
    // Unique local (fc00::/7) and link-local (fe80::/10) IPv6.
    return /^f[cd][0-9a-f]{2}:/.test(host) || /^fe[89ab][0-9a-f]:/.test(host)
  }
  // A single-label name ("nas") only resolves on a local network.
  return !host.includes('.') || PRIVATE_SUFFIXES.some((s) => host.endsWith(s))
}

export function consoleReach(origin: string): ConsoleReach {
  let hostname: string
  try {
    hostname = new URL(origin).hostname
  } catch {
    return 'public'
  }
  if (isLoopbackHost(hostname)) return 'loopback'
  if (isPrivateHost(hostname)) return 'private'
  return 'public'
}
