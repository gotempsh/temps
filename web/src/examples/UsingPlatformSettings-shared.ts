// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
