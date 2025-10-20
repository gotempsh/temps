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

// Re-export the types from the API for consistency
export interface PlatformSettings extends AppSettings {
  dns_provider: DnsProviderSettings
  external_url: string | null
  letsencrypt: LetsEncryptSettings
  preview_domain: string
  screenshots: ScreenshotSettings
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
      // Ensure all required fields have defaults
      const settings: PlatformSettings = {
        dns_provider: response.data.dns_provider || {
          provider: 'manual',
          cloudflare_api_key: null,
        },
        external_url: response.data.external_url || null,
        letsencrypt: response.data.letsencrypt || {
          email: null,
          environment: 'production',
        },
        preview_domain: response.data.preview_domain || 'localho.st',
        screenshots: response.data.screenshots || {
          enabled: false,
          provider: 'local',
          url: null,
        },
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

    // Update via API
    const response = await updateSettings({
      body: {
        dns_provider: updated.dns_provider,
        external_url: updated.external_url,
        letsencrypt: updated.letsencrypt,
        preview_domain: updated.preview_domain,
        screenshots: updated.screenshots,
      },
    })

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
