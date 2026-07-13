// Shared pure helpers for the email management UI.
// Extracted from EmailsSentList / EmailDetail / EmailDomainsManagement /
// EmailDomainDetail / EmailProvidersManagement / EmailEventTimeline /
// EmailAnalytics to avoid copy-pasted logic drifting out of sync.

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
  if (ua.includes('GoogleImageProxy') || ua.includes('ggpht.com')) return 'Gmail (Google Proxy)'
  if (ua.includes('YahooMailProxy')) return 'Yahoo Mail (Proxy)'
  if (ua.includes('Outlook-iOS') || ua.includes('Outlook-Android')) return 'Outlook Mobile'
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
