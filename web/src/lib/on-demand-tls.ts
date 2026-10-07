// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Console-side view of the on-demand TLS settings (ADR-018).
 *
 * The proxy reads `on_demand_tls` once at startup and silently disables the
 * feature when it cannot work (loopback external URL, no zone). These helpers
 * mirror those startup checks (`crates/temps-cli/src/commands/serve/
 * on_demand_cert.rs`) so the console can say *why* issuance would not start
 * instead of showing a switch that is on but does nothing.
 */

export const ON_DEMAND_TLS_SETTINGS_PATH = '/settings/on-demand-tls'

/** Where the Let's Encrypt contact email is edited. */
export const CERTIFICATE_SETTINGS_PATH = '/settings'

export const DEPLOYMENT_URL_MODES = ['http', 'redirect_to_env'] as const
export type DeploymentUrlMode = (typeof DEPLOYMENT_URL_MODES)[number]

/** Same bounds the form enforces; the proxy clamps anything below 1 to 1. */
export const MAX_CONCURRENT_RANGE = { min: 1, max: 20 } as const
export const HOURLY_CAP_RANGE = { min: 1, max: 300 } as const

interface OnDemandTlsLike {
  enabled?: boolean
  zone?: string | null
}

export interface OnDemandTlsSettingsSource {
  on_demand_tls?: OnDemandTlsLike | null
  external_url?: string | null
  letsencrypt?: { email?: string | null } | null
}

export type OnDemandTlsState = 'enabled' | 'disabled' | 'unknown'

/**
 * `unknown` covers a settings read that is still loading or failed — notably
 * a 403 for a user who may list certificates (DomainsRead) but not read
 * settings (SettingsRead). The console must not claim the feature is off then.
 */
export function onDemandTlsState(
  settings: OnDemandTlsSettingsSource | undefined,
  readFailed: boolean
): OnDemandTlsState {
  if (readFailed || !settings) return 'unknown'
  return settings.on_demand_tls?.enabled ? 'enabled' : 'disabled'
}

export interface CertificatesEmptyStateCopy {
  title: string
  description: string
  action: { label: string; href: string } | null
}

export function certificatesEmptyStateCopy(
  state: OnDemandTlsState
): CertificatesEmptyStateCopy {
  switch (state) {
    case 'enabled':
      return {
        title: 'No certificate attempts yet',
        description:
          'On-demand TLS is on. An attempt is recorded here the first time a hostname routed through the proxy is requested over HTTPS.',
        action: {
          label: 'On-demand TLS settings',
          href: ON_DEMAND_TLS_SETTINGS_PATH,
        },
      }
    case 'disabled':
      return {
        title: 'On-demand TLS is off',
        description:
          "When it is on, the proxy requests a Let's Encrypt certificate the first time a routed hostname is visited over HTTPS, and every attempt is listed here. Nothing is issued automatically while it is off.",
        action: {
          label: 'Turn on on-demand TLS',
          href: ON_DEMAND_TLS_SETTINGS_PATH,
        },
      }
    case 'unknown':
      return {
        title: 'No certificate attempts yet',
        description:
          'Attempts appear here once a hostname routed through the proxy is requested over HTTPS. Whether on-demand TLS is turned on is shown under Settings → On-demand TLS, which needs permission to read platform settings.',
        action: null,
      }
  }
}

function hostOf(url: string): string | null {
  const trimmed = url.trim()
  if (!trimmed) return null
  try {
    return new URL(trimmed).hostname.toLowerCase()
  } catch {
    // A bare host without a scheme, as the backend also accepts.
    return trimmed.split('/')[0].split(':')[0].toLowerCase() || null
  }
}

/** Mirrors `external_url_is_loopback` in the proxy startup wiring. */
export function isLoopbackHost(host: string): boolean {
  const h = host
    .replace(/\.$/, '')
    .replace(/^\[|\]$/g, '')
    .toLowerCase()
  return (
    h === 'localhost' ||
    h.endsWith('.localhost') ||
    h.startsWith('127.') ||
    h.includes('127-0-0-1') ||
    h === '::1'
  )
}

/**
 * Mirrors `derive_zone`: an explicit zone wins, otherwise a `*.sslip.io`
 * external URL is its own zone. `null` means the proxy would disable the
 * feature at startup.
 */
export function effectiveZone(
  zone: string | null | undefined,
  externalUrl: string | null | undefined
): string | null {
  const configured = zone?.trim().replace(/\.$/, '').toLowerCase()
  if (configured) return configured
  const host = externalUrl ? hostOf(externalUrl) : null
  if (!host) return null
  const normalized = host.replace(/\.$/, '')
  return normalized.endsWith('sslip.io') ? normalized : null
}

export interface OnDemandTlsBlocker {
  id: 'loopback' | 'no-zone' | 'no-email'
  message: string
  fixLabel: string | null
  fixHref: string | null
}

/**
 * Reasons the proxy would refuse to issue even with the switch on. Evaluated
 * against the values in the form (zone) plus the stored platform settings.
 */
export function onDemandTlsBlockers(
  zone: string | null | undefined,
  settings: OnDemandTlsSettingsSource
): OnDemandTlsBlocker[] {
  const blockers: OnDemandTlsBlocker[] = []
  const externalHost = settings.external_url
    ? hostOf(settings.external_url)
    : null
  if (externalHost && isLoopbackHost(externalHost)) {
    blockers.push({
      id: 'loopback',
      message: `The external URL points at ${externalHost}, a loopback address. Let's Encrypt cannot reach it to complete the HTTP-01 challenge, so the proxy keeps on-demand TLS off.`,
      fixLabel: 'Change the external URL',
      fixHref: CERTIFICATE_SETTINGS_PATH,
    })
  }
  if (!effectiveZone(zone, settings.external_url)) {
    blockers.push({
      id: 'no-zone',
      message:
        'No zone is set and none can be derived: the external URL is not a *.sslip.io address. Enter the domain whose direct subdomains should receive certificates.',
      fixLabel: null,
      fixHref: null,
    })
  }
  if (!settings.letsencrypt?.email?.trim()) {
    blockers.push({
      id: 'no-email',
      message:
        "No Let's Encrypt contact email is set, so every issuance attempt fails.",
      fixLabel: 'Set the contact email',
      fixHref: CERTIFICATE_SETTINGS_PATH,
    })
  }
  return blockers
}

/** Empty or whitespace becomes `null` ("derive from the external URL"). */
export function normalizeZoneInput(
  zone: string | null | undefined
): string | null {
  const trimmed = zone?.trim().replace(/\.$/, '').toLowerCase() ?? ''
  return trimmed.length > 0 ? trimmed : null
}

/**
 * A zone is a bare DNS name: no scheme, path, port or wildcard. Empty is
 * valid (auto-derive).
 */
export function isValidZoneInput(zone: string | null | undefined): boolean {
  const normalized = normalizeZoneInput(zone)
  if (normalized === null) return true
  if (normalized.length > 253) return false
  return normalized
    .split('.')
    .every(
      (label) =>
        label.length > 0 &&
        label.length <= 63 &&
        /^[a-z0-9]([a-z0-9-]*[a-z0-9])?$/.test(label)
    )
}
