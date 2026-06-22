import { getSettings, updateSettings } from '@/api/client'
import type {
  AppSettingsResponse,
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

/// Control-plane build resource limits. Mirrors the Rust
/// `BuildLimitsSettings` struct. `cpu_limit_cores = 0` or `memory_limit_mb
/// = 0` means "fall back to the legacy 50%-of-host heuristic" so admins
/// can enable concurrency without committing to specific resource caps.
export interface BuildLimitsSettings {
  max_concurrent: number
  cpu_limit_cores: number
  memory_limit_mb: number
}

/// Per-provider config as returned by GET /api/settings. The `credentials_encrypted`
/// blob is never sent over the wire — the server replaces it with a boolean
/// `credential_saved` so we never ship ciphertext to the browser. On PUT the
/// server preserves stored credentials when the incoming provider entry
/// doesn't include a fresh one.
export interface ProviderConfigMasked {
  auth_type: string
  credential_saved: boolean
  default_model: string | null
  extra: unknown
}

export interface AgentSandboxSettings {
  default_provider: string
  providers?: Record<string, ProviderConfigMasked>
  /// True if a legacy top-level credential is stored. The ciphertext itself
  /// is never returned.
  api_key_saved?: boolean
  /// @deprecated Per-provider auth lives in `providers[id].auth_type` now.
  auth_type?: string
  /// @deprecated Per-provider model lives in `providers[id].default_model` now.
  default_model?: string
  enabled: boolean
  runtime: string
  custom_image: string
  cpu_limit: number
  memory_limit_mb: number
  network_mode: string
}

export interface PreviewGatewaySettings {
  image: string
  host_port: number
  auto_upgrade: boolean
  shared_secret_set: boolean
}

export interface MultiNodeSettings {
  has_join_token: boolean
  private_address: string | null
}

export interface AiConfigSettings {
  config_repo: string
  config_repo_branch: string
}

export type MetricsStoreKind = 'timescale_db' | 'click_house'

export interface MonitoringSettings {
  enabled: boolean
  store: MetricsStoreKind
  /** How often the MetricsScraper collects data, in seconds. Min 15. */
  scrape_interval_secs: number
  /** Days of raw (30s resolution) data to keep. Min 1, max 30. */
  retention_raw_days: number
  /** Days of hourly-aggregate data to keep. Min 7, max 365. */
  retention_hourly_days: number
  /** Years of daily-aggregate data to keep. */
  retention_daily_years: number
  /**
   * True when a ClickHouse DSN is stored. The GET response masks the DSN
   * itself (it may embed credentials), so reads only expose this boolean.
   */
  clickhouse_url_set: boolean
  /**
   * ClickHouse DSN — only sent on writes when configuring ClickHouse. Never
   * returned by the GET response (see `clickhouse_url_set`). The server
   * preserves the stored DSN when this is omitted on an update.
   */
  clickhouse_url?: string | null
}

/** Per-managed-domain hostname layout (configured under DNS providers, not here). */
export type PublicHostnameStrategy = 'standard' | 'flat'

// Re-export the types from the API for consistency
export interface PlatformSettings extends AppSettingsResponse {
  external_url: string | null
  internal_url: string | null
  letsencrypt: LetsEncryptSettings
  preview_domain: string
  /** Public address synced DNS records point at (IP → A/AAAA, else CNAME). */
  edge_target?: string | null
  screenshots: ScreenshotSettings
  security_headers: SecurityHeadersSettings
  rate_limiting: RateLimitSettings
  disk_space_alert: DiskSpaceAlertSettings
  ai_config: AiConfigSettings
  insecure_tls: boolean
  attack_mode?: boolean
  build_limits: BuildLimitsSettings
  /** Set to true by `temps setup` once initial configuration has been applied.
   * The web onboarding wizard checks this and skips itself when true. */
  setup_complete: boolean
}

/**
 * Get platform settings.
 *
 * The server is the single source of truth — every field on `AppSettings`
 * (including `agent_sandbox`, `ai_config`, security headers, etc.) is
 * populated server-side via `AppSettings::default()` when the row is
 * created. This client used to re-default each field when missing, which
 * masked real backend bugs (e.g. the user activating Codex but the UI
 * silently rendering `claude_cli` because some unrelated field was null).
 *
 * Errors propagate to the caller so TanStack Query can surface a real
 * error state instead of substituting hardcoded defaults.
 */
export async function getPlatformSettings(): Promise<PlatformSettings> {
  const response = await getSettings()

  if (!response.data) {
    throw new Error('Settings endpoint returned no data')
  }

  // Cast to include extended fields not yet present in generated OpenAPI types.
  // The server contract guarantees these are populated.
  return response.data as PlatformSettings
}

/**
 * Update platform settings. Reads the current full settings from the server,
 * merges the partial update on top, validates, and persists. No localStorage —
 * always round-trip through the server so we never write stale cached data.
 */
export async function updatePlatformSettings(
  settings: Partial<PlatformSettings>
): Promise<PlatformSettings> {
  const current = await getPlatformSettings()
  const updated = { ...current, ...settings }

  validateSettings(updated)

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const body: any = {
    dns_provider: updated.dns_provider,
    external_url: updated.external_url,
    internal_url: updated.internal_url,
    letsencrypt: updated.letsencrypt,
    preview_domain: updated.preview_domain,
    edge_target: updated.edge_target,
    screenshots: updated.screenshots,
    security_headers: updated.security_headers,
    rate_limiting: updated.rate_limiting,
    disk_space_alert: updated.disk_space_alert,
    agent_sandbox: updated.agent_sandbox,
    ai_config: updated.ai_config,
    attack_mode: updated.attack_mode,
    build_limits: updated.build_limits,
    monitoring: updated.monitoring,
  }
  const result = await updateSettings({ body })
  if (result.error) {
    // The generated client resolves (rather than throws) on non-2xx
    // responses, so a failed save must be surfaced explicitly here —
    // otherwise the caller sees a resolved promise and reports "saved"
    // for a change that was actually rejected (e.g. a validation error).
    const detail =
      (result.error as { detail?: string })?.detail || 'Failed to save settings'
    throw new Error(detail)
  }

  // The PATCH endpoint returns only an ack message, so we hand back our
  // merged view. Callers that need the absolute server state should refetch.
  return updated
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
