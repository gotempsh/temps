// Shared pure helpers for the email management UI.
// Extracted from EmailsSentList / EmailDetail / EmailDomainsManagement /
// EmailDomainDetail / EmailProvidersManagement / EmailEventTimeline /
// EmailAnalytics to avoid copy-pasted logic drifting out of sync.

import { deleteEmailProvider as deleteEmailProviderApi } from '@/api/client'

export function problemMessage(error: unknown, fallback: string): string {
  if (error && typeof error === 'object' && 'detail' in error) {
    const detail = (error as { detail?: unknown }).detail
    if (typeof detail === 'string' && detail.length > 0) {
      return detail
    }
  }
  return fallback
}

export function parseUserAgent(ua: string): string {
  // Email image proxies (check first — they masquerade as browsers)
  if (ua.includes('GoogleImageProxy') || ua.includes('ggpht.com'))
    return 'Gmail (Google Proxy)'
  if (ua.includes('YahooMailProxy')) return 'Yahoo Mail (Proxy)'
  if (ua.includes('Outlook-iOS') || ua.includes('Outlook-Android'))
    return 'Outlook Mobile'
  // Email clients
  if (ua.includes('Gmail')) return 'Gmail'
  if (ua.includes('Yahoo')) return 'Yahoo Mail'
  if (ua.includes('Outlook') || ua.includes('Microsoft')) return 'Outlook'
  if (ua.includes('Thunderbird')) return 'Thunderbird'
  if (ua.includes('Apple Mail')) return 'Apple Mail'
  // Browsers
  if (ua.includes('Chrome') && !ua.includes('Chromium')) return 'Chrome'
  if (ua.includes('Firefox')) return 'Firefox'
  if (ua.includes('Safari') && !ua.includes('Chrome')) return 'Safari'
  if (ua.includes('AppleWebKit')) return 'WebKit'
  if (ua.length > 50) return ua.substring(0, 50) + '...'
  return ua
}

export async function deleteEmailProvider(id: number): Promise<void> {
  const response = await deleteEmailProviderApi({ path: { id } })
  if (response.error) {
    throw new Error(
      problemMessage(response.error, 'Failed to delete email provider')
    )
  }
}

/**
 * The backend returns credentials as a freeform JSON value masked for display
 * (e.g. `{"host":"...", "port":587, "encryption":"starttls", "username":"AKIA...XYZ"}`).
 * This pulls out only the non-secret fields we need to prefill the edit form.
 */
export function readMaskedCreds(credentials: unknown): {
  host?: string
  port?: number
  encryption?: 'starttls' | 'tls' | 'none'
  accept_invalid_certs?: boolean
} {
  if (!credentials || typeof credentials !== 'object') return {}
  const c = credentials as Record<string, unknown>
  const enc = typeof c.encryption === 'string' ? c.encryption : undefined
  return {
    host: typeof c.host === 'string' ? c.host : undefined,
    port: typeof c.port === 'number' ? c.port : undefined,
    encryption:
      enc === 'starttls' || enc === 'tls' || enc === 'none' ? enc : undefined,
    accept_invalid_certs:
      typeof c.accept_invalid_certs === 'boolean'
        ? c.accept_invalid_certs
        : undefined,
  }
}
