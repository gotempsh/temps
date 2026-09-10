// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { credentials } from '../config/store.js'
import { colors } from '../ui/output.js'

const CLOUD_URL = process.env.TEMPS_CLOUD_URL ?? 'https://temps.sh'

export function getCloudUrl(): string {
  return CLOUD_URL
}

export async function requireCloudAuth(): Promise<string> {
  const envToken = process.env.TEMPS_CLOUD_TOKEN || process.env.TEMPS_CLOUD_API_KEY
  if (envToken) {
    return envToken
  }

  const key = await credentials.get('cloudApiKey')
  if (!key) {
    console.error(colors.error('Not logged in to Temps Cloud. Run: temps cloud login'))
    process.exit(1)
  }
  return key
}

export async function isCloudAuthenticated(): Promise<boolean> {
  const envToken = process.env.TEMPS_CLOUD_TOKEN || process.env.TEMPS_CLOUD_API_KEY
  if (envToken) return true

  const key = await credentials.get('cloudApiKey')
  return !!key
}

export async function cloudFetch<T>(path: string, options?: RequestInit): Promise<T> {
  const apiKey = await requireCloudAuth()
  const response = await fetch(`${CLOUD_URL}${path}`, {
    ...options,
    headers: {
      'Authorization': `Bearer ${apiKey}`,
      'Content-Type': 'application/json',
      ...options?.headers,
    },
  })

  if (!response.ok) {
    const body = await response.json().catch(() => ({})) as Record<string, unknown>
    const message = (body.error as string) || (body.error_description as string) || (body.detail as string) || (body.message as string) || `Request failed (${response.status})`
    throw new Error(message)
  }

  const text = await response.text()
  if (!text) return undefined as T
  return JSON.parse(text) as T
}

export async function cloudFetchPublic<T>(path: string): Promise<T> {
  const response = await fetch(`${CLOUD_URL}${path}`, {
    headers: { 'Content-Type': 'application/json' },
  })

  if (!response.ok) {
    const body = await response.json().catch(() => ({})) as Record<string, unknown>
    const message = (body.error as string) || (body.detail as string) || `Request failed (${response.status})`
    throw new Error(message)
  }

  return response.json() as Promise<T>
}
