import { getSettings, updateSettings } from '@/api/client'
import type {
  AppSettings,
  DnsProviderSettings,
  LetsEncryptSettings,
  ScreenshotSettings,
} from '@/api/client/types.gen'

/**
 * Platform Settings API Service
 *
 * This service handles all platform configuration settings.
 * Uses the actual backend API endpoints.
 */

// Security settings types (matching backend)
export interface SecurityHeadersSettings {
  enabled: boolean
  preset: string
  content_security_policy: string | null
  x_frame_options: string
  x_content_type_options: string
  x_xss_protection: string
  strict_transport_security: string
  referrer_policy: string
  permissions_policy: string | null
}

export interface RateLimitSettings {
  enabled: boolean
  max_requests_per_minute: number
  max_requests_per_hour: number
  whitelist_ips: string[]
  blacklist_ips: string[]
}

export interface DiskSpaceAlertSettings {
  enabled: boolean
  threshold_percent: number
  check_interval_seconds: number
  monitor_path: string | null
}

// Re-export the types from the API for consistency
export interface PlatformSettings extends AppSettings {
  dns_provider: DnsProviderSettings
  external_url: string | null
  letsencrypt: LetsEncryptSettings
  preview_domain: string
  screenshots: ScreenshotSettings
  security_headers: SecurityHeadersSettings
  rate_limiting: RateLimitSettings
  disk_space_alert: DiskSpaceAlertSettings
  attack_mode?: boolean
}

/**
 * Get platform settings
 * @returns Promise<PlatformSettings>
 */
export async function getPlatformSettings(): Promise<PlatformSettings> {
  try {
    // Fetch from API
    const response = await getSettings()

    if (response.data) {
      // Cast to include extended fields not yet in generated types
      const data = response.data as typeof response.data & {
        disk_space_alert?: DiskSpaceAlertSettings
        attack_mode?: boolean
      }
      // Ensure all required fields have defaults
      const settings: PlatformSettings = {
        dns_provider: data.dns_provider || {
          provider: 'manual',
          cloudflare_api_key: null,
        },
        external_url: data.external_url || null,
        letsencrypt: data.letsencrypt || {
          email: null,
          environment: 'production',
        },
        preview_domain: data.preview_domain || 'localho.st',
        screenshots: data.screenshots || {
          enabled: false,
          provider: 'local',
          url: null,
        },
        security_headers: data.security_headers || {
          enabled: true,
          preset: 'moderate',
          content_security_policy:
            "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'",
          x_frame_options: 'SAMEORIGIN',
          x_content_type_options: 'nosniff',
          x_xss_protection: '1; mode=block',
          strict_transport_security: 'max-age=31536000; includeSubDomains',
          referrer_policy: 'strict-origin-when-cross-origin',
          permissions_policy: 'geolocation=(), microphone=(), camera=()',
        },
        rate_limiting: data.rate_limiting || {
          enabled: false,
          max_requests_per_minute: 60,
          max_requests_per_hour: 1000,
          whitelist_ips: [],
          blacklist_ips: [],
        },
        disk_space_alert: data.disk_space_alert || {
          enabled: true,
          threshold_percent: 80,
          check_interval_seconds: 300,
          monitor_path: null,
        },
        attack_mode: data.attack_mode || false,
      }

      // Cache in localStorage for offline access
      localStorage.setItem('platform_settings', JSON.stringify(settings))
      return settings
    }

    // Return default settings if no data
    return getDefaultSettings()
  } catch (error) {
    console.error('Error fetching platform settings:', error)
    // Return cached or default settings on error
    const stored = localStorage.getItem('platform_settings')
    if (stored) {
      return JSON.parse(stored)
    }
    return getDefaultSettings()
  }
}

/**
 * Update platform settings
 * @param settings - Partial settings to update
 * @returns Promise<PlatformSettings>
 */
export async function updatePlatformSettings(
  settings: Partial<PlatformSettings>
): Promise<PlatformSettings> {
  try {
    // Get current settings from cache first to avoid overwriting with API defaults
    const cachedSettings = localStorage.getItem('platform_settings')
    const current = cachedSettings
      ? JSON.parse(cachedSettings)
      : await getPlatformSettings()
    const updated = { ...current, ...settings }

    // Validate settings before saving
    validateSettings(updated)

    // Update via API - cast body to include extended fields not yet in generated types
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const body: any = {
      dns_provider: updated.dns_provider,
      external_url: updated.external_url,
      letsencrypt: updated.letsencrypt,
      preview_domain: updated.preview_domain,
      screenshots: updated.screenshots,
      security_headers: updated.security_headers,
      rate_limiting: updated.rate_limiting,
      disk_space_alert: updated.disk_space_alert,
      attack_mode: updated.attack_mode,
    }
    const response = await updateSettings({ body })

    if (response.data) {
      // API returns only a message, so use our updated settings
      // Update localStorage cache with the settings we sent
      localStorage.setItem('platform_settings', JSON.stringify(updated))
      return updated
    }

    return updated
  } catch (error) {
    console.error('Error updating platform settings:', error)
    throw error
  }
}

/**
 * Validate platform settings
 * @param settings - Settings to validate
 * @throws Error if settings are invalid
 */
function validateSettings(settings: PlatformSettings): void {
  // Validate external URL format
  if (settings.external_url && !isValidUrl(settings.external_url)) {
    throw new Error('Invalid external URL format')
  }

  // Validate preview domain format
  if (!settings.preview_domain || settings.preview_domain.length < 3) {
    throw new Error('Preview domain must be at least 3 characters')
  }

  // Validate Cloudflare API key if provider is cloudflare
  if (
    settings.dns_provider?.provider === 'cloudflare' &&
    !settings.dns_provider.cloudflare_api_key
  ) {
    throw new Error(
      'Cloudflare API key is required when using Cloudflare DNS provider'
    )
  }

  // Validate Let's Encrypt email
  if (
    settings.letsencrypt?.email &&
    !isValidEmail(settings.letsencrypt.email)
  ) {
    throw new Error("Invalid Let's Encrypt email format")
  }

  // Validate screenshot URL if external provider
  if (
    settings.screenshots?.enabled &&
    settings.screenshots.provider === 'external' &&
    (!settings.screenshots.url || !isValidUrl(settings.screenshots.url))
  ) {
    throw new Error(
      'Valid screenshot API URL is required when using external provider'
    )
  }
}

/**
 * Get default platform settings
 * @returns PlatformSettings
 */
function getDefaultSettings(): PlatformSettings {
  return {
    dns_provider: {
      provider: 'manual',
      cloudflare_api_key: null,
    },
    external_url: null,
    letsencrypt: {
      email: null,
      environment: 'production',
    },
    preview_domain: 'localho.st',
    screenshots: {
      enabled: false,
      provider: 'local',
      url: '',
    },
    security_headers: {
      enabled: true,
      preset: 'moderate',
      content_security_policy:
        "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'",
      x_frame_options: 'SAMEORIGIN',
      x_content_type_options: 'nosniff',
      x_xss_protection: '1; mode=block',
      strict_transport_security: 'max-age=31536000; includeSubDomains',
      referrer_policy: 'strict-origin-when-cross-origin',
      permissions_policy: 'geolocation=(), microphone=(), camera=()',
    },
    rate_limiting: {
      enabled: false,
      max_requests_per_minute: 60,
      max_requests_per_hour: 1000,
      whitelist_ips: [],
      blacklist_ips: [],
    },
    disk_space_alert: {
      enabled: true,
      threshold_percent: 80,
      check_interval_seconds: 300,
      monitor_path: null,
    },
    attack_mode: false,
  }
}

/**
 * Validate URL format
 * @param url - URL to validate
 * @returns boolean
 */
function isValidUrl(url: string): boolean {
  try {
    new URL(url)
    return true
  } catch {
    return false
  }
}

/**
 * Validate email format
 * @param email - Email to validate
 * @returns boolean
 */
function isValidEmail(email: string): boolean {
  const emailRegex = /^[^\s@]+@[^\s@]+\.[^\s@]+$/
  return emailRegex.test(email)
}

// Export individual setting getters for convenience
export async function getDnsProvider(): Promise<
  PlatformSettings['dns_provider']
> {
  const settings = await getPlatformSettings()
  return settings.dns_provider
}

export async function getExternalUrl(): Promise<string | null> {
  const settings = await getPlatformSettings()
  return settings.external_url
}

export async function getLetsEncryptConfig(): Promise<
  PlatformSettings['letsencrypt']
> {
  const settings = await getPlatformSettings()
  return settings.letsencrypt
}

export async function getPreviewDomain(): Promise<string> {
  const settings = await getPlatformSettings()
  return settings.preview_domain
}

export async function getScreenshotsConfig(): Promise<
  PlatformSettings['screenshots']
> {
  const settings = await getPlatformSettings()
  return settings.screenshots
}
