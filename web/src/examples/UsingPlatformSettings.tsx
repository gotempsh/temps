/**
 * Example: How to use Platform Settings in your components
 *
 * This file demonstrates various ways to access and use platform settings
 * throughout your application.
 */

import { useSettings } from '@/hooks/useSettings'
import {
  useDnsProvider,
  useExternalUrl,
  usePreviewDomain,
  useScreenshots,
} from '@/hooks/usePlatformConfig'

// Example 1: Using the full settings object
export function ExampleFullSettings() {
  const { data: settings, isLoading } = useSettings()

  if (isLoading) return <div>Loading settings...</div>

  return (
    <div>
      <h3>Platform Configuration</h3>
      <ul>
        <li>External URL: {settings?.external_url || 'Not configured'}</li>
        <li>DNS Provider: {settings?.dns_provider?.provider || 'manual'}</li>
        <li>Preview Domain: {settings?.preview_domain}</li>
        <li>
          Screenshots: {settings?.screenshots?.enabled ? 'Enabled' : 'Disabled'}
        </li>
      </ul>
    </div>
  )
}

// Example 2: Using individual setting hooks
export function ExampleIndividualSettings() {
  const { data: externalUrl } = useExternalUrl()
  const { data: dnsProvider } = useDnsProvider()
  const { data: previewDomain } = usePreviewDomain()

  return (
    <div>
      <p>External URL: {externalUrl || 'Not set'}</p>
      <p>
        DNS: {dnsProvider?.provider === 'cloudflare' ? 'Cloudflare' : 'Manual'}
      </p>
      <p>Preview: {previewDomain}</p>
    </div>
  )
}

// Example 3: Building URLs with settings
export function ExampleWebhookUrl() {
  const { data: externalUrl } = useExternalUrl()

  if (!externalUrl) {
    return <div>Please configure external URL in settings</div>
  }

  const webhookUrl = `${externalUrl}/api/webhooks/github`
  const oauthCallbackUrl = `${externalUrl}/auth/callback`

  return (
    <div>
      <p>Webhook URL: {webhookUrl}</p>
      <p>OAuth Callback: {oauthCallbackUrl}</p>
    </div>
  )
}

// Example 4: Building preview URLs
export function ExamplePreviewUrl({ deploymentId }: { deploymentId: string }) {
  const { data: previewDomain } = usePreviewDomain()

  const previewUrl = `https://${deploymentId}.${previewDomain}`

  return (
    <a href={previewUrl} target="_blank" rel="noopener noreferrer">
      View Preview: {previewUrl}
    </a>
  )
}

// Example 5: Conditional features based on settings
export function ExampleScreenshotFeature() {
  const { data: screenshots } = useScreenshots()

  if (!screenshots?.enabled) {
    return null // Don't show screenshot features if disabled
  }

  return (
    <div>
      <h3>Screenshot Preview</h3>
      <p>Provider: {screenshots.provider}</p>
      {screenshots.provider === 'external' && <p>API: {screenshots.url}</p>}
      {/* Screenshot functionality here */}
    </div>
  )
}

// Example 6: DNS configuration check
export function ExampleDnsCheck() {
  const { data: dnsProvider } = useDnsProvider()

  if (
    dnsProvider?.provider === 'cloudflare' &&
    dnsProvider.cloudflare_api_key
  ) {
    return (
      <div className="text-green-600">
        ✓ Automatic DNS management enabled via Cloudflare
      </div>
    )
  }

  return (
    <div className="text-yellow-600">⚠ Manual DNS configuration required</div>
  )
}

// Example 7: Using settings in API calls
export async function createGitHubWebhook(repoName: string) {
  // Get the external URL from settings
  const { getPlatformSettings } = await import('@/api/platformSettings')
  const settings = await getPlatformSettings()

  if (!settings.external_url) {
    throw new Error('External URL not configured')
  }

  const webhookUrl = `${settings.external_url}/api/webhooks/github`

  // Create webhook with GitHub API
  const response = await fetch(
    `https://api.github.com/repos/${repoName}/hooks`,
    {
      method: 'POST',
      headers: {
        Authorization: `token ${process.env.GITHUB_TOKEN}`,
        'Content-Type': 'application/json',
      },
      body: JSON.stringify({
        config: {
          url: webhookUrl,
          content_type: 'json',
        },
        events: ['push', 'pull_request'],
      }),
    }
  )

  return response.json()
}
