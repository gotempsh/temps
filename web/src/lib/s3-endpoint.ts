// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Preview of how the server will store and use an S3 endpoint while the
 * operator is still typing it. Mirrors `temps_core::s3_endpoint`: a bare
 * `host:port` is plain HTTP, an IP-address host forces path-style
 * addressing. The server is authoritative; this only explains in the form
 * what saving will do.
 */

export type S3EndpointScheme = 'http' | 'https'

export interface S3EndpointPreview {
  /** The URL every client (WAL-G, restores, worker jobs) will receive. */
  url: string
  scheme: S3EndpointScheme
  /** Whether the operator typed the scheme. */
  explicitScheme: boolean
  /** Whether the host is an IPv4 or IPv6 literal. */
  hostIsIp: boolean
}

const IPV4 = /^(25[0-5]|2[0-4]\d|1?\d?\d)(\.(25[0-5]|2[0-4]\d|1?\d?\d)){3}$/

/** `null` for an empty value (AWS S3 itself) or one the server will reject. */
export function previewS3Endpoint(
  raw: string | null | undefined
): S3EndpointPreview | null {
  const trimmed = (raw ?? '').trim()
  if (!trimmed) return null
  const schemeMatch = /^([a-z][a-z0-9+.-]*):\/\//i.exec(trimmed)
  if (schemeMatch) {
    const scheme = schemeMatch[1].toLowerCase()
    if (scheme !== 'http' && scheme !== 'https') return null
  }
  const candidate = schemeMatch ? trimmed : `http://${trimmed}`
  let parsed: URL
  try {
    parsed = new URL(candidate)
  } catch {
    return null
  }
  if (!parsed.hostname || parsed.username || parsed.password) return null
  const scheme: S3EndpointScheme =
    parsed.protocol === 'https:' ? 'https' : 'http'
  const host = parsed.hostname.replace(/^\[|\]$/g, '')
  const hostIsIp = IPV4.test(host) || host.includes(':')
  const path = parsed.pathname.replace(/\/+$/, '')
  return {
    url: `${scheme}://${parsed.host}${path}`,
    scheme,
    explicitScheme: Boolean(schemeMatch),
    hostIsIp,
  }
}

export type PathStyleReason =
  'configured' | 'ip_address_endpoint' | 'managed_object_store'

/** The sentence shown next to the path-style setting. */
export function describePathStyleReason(reason: PathStyleReason): string {
  switch (reason) {
    case 'ip_address_endpoint':
      return 'Forced: the endpoint is an IP address, where bucket.host addressing cannot resolve.'
    case 'managed_object_store':
      return 'Forced: the endpoint is a Temps-managed object store, which serves path-style requests only.'
    case 'configured':
      return 'As configured on this source.'
  }
}

/** Path style as the server will apply it for a form's current values. */
export function previewPathStyle(
  endpoint: string | null | undefined,
  configured: boolean,
  managedObjectStore = false
): { forcePathStyle: boolean; reason: PathStyleReason } {
  const forced: PathStyleReason | null = previewS3Endpoint(endpoint)?.hostIsIp
    ? 'ip_address_endpoint'
    : managedObjectStore
      ? 'managed_object_store'
      : null
  if (forced && !configured) return { forcePathStyle: true, reason: forced }
  return { forcePathStyle: configured, reason: 'configured' }
}
