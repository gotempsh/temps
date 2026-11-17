// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export function parseProxyRanges(text: string): string[] {
  return text
    .split(/[\n,]/)
    .map((entry) => entry.trim())
    .filter(Boolean)
}

function ipv4(address: string): boolean {
  const octets = address.split('.')
  return (
    octets.length === 4 &&
    octets.every(
      (part) => /^(0|[1-9]\d{0,2})$/.test(part) && Number(part) <= 255
    )
  )
}

function ipv6(address: string): boolean {
  // The URL parser validates IPv6 groups and embedded IPv4 without accepting
  // a port, zone ID or brackets in the operator's address itself.
  if (!/^[\da-fA-F:.]+$/.test(address) || !address.includes(':')) return false
  try {
    return new URL(`http://[${address}]/`).hostname.startsWith('[')
  } catch {
    return false
  }
}

export function validateProxyRanges(text: string): true | string {
  const entries = parseProxyRanges(text)
  if (entries.length > 64)
    return 'Use at most 64 trusted proxy addresses or ranges.'
  for (const entry of entries) {
    const [address, prefix, extra] = entry.split('/')
    const v4 = ipv4(address)
    if (entry.length > 64 || extra !== undefined || (!v4 && !ipv6(address))) {
      return `Enter a valid IP address or CIDR: ${entry}`
    }
    if (
      prefix !== undefined &&
      (!/^\d{1,3}$/.test(prefix) ||
        Number(prefix) < 1 ||
        Number(prefix) > (v4 ? 32 : 128))
    ) {
      return `Use a prefix from 1 to ${v4 ? 32 : 128}: ${entry}`
    }
    if (!v4 && prefix === '96') {
      const host = new URL(`http://[${address}]/`).hostname
      if (host.startsWith('[::ffff:'))
        return 'A mapped /96 trusts every IPv4 address. Specify narrower proxy ranges.'
    }
  }
  return true
}
