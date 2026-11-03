// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  getEmailStats,
  getGlobalEventStats,
  listEmailDomains,
  listEmailProviders,
  type EmailDomainResponse,
  type EmailProviderResponse,
  type EmailStatsResponse,
  type GlobalEventStatsResponse,
} from '@/api/client'
import { problemMessage } from './sharedUtils'

export async function fetchEmailDomains(): Promise<EmailDomainResponse[]> {
  const response = await listEmailDomains()
  if (response.error) {
    throw new Error(
      problemMessage(response.error, 'Failed to fetch email domains')
    )
  }
  return response.data ?? []
}

export async function fetchEmailProviders(): Promise<EmailProviderResponse[]> {
  const response = await listEmailProviders()
  if (response.error) {
    throw new Error(
      problemMessage(response.error, 'Failed to fetch email providers')
    )
  }
  return response.data ?? []
}

export async function fetchEmailStats(
  domainId?: number
): Promise<EmailStatsResponse> {
  const response = await getEmailStats(
    domainId !== undefined ? { query: { domain_id: domainId } } : undefined
  )
  if (response.error || !response.data) {
    throw new Error(
      problemMessage(response.error, 'Failed to fetch email stats')
    )
  }
  return response.data
}

export async function fetchEventStats(): Promise<GlobalEventStatsResponse> {
  const response = await getGlobalEventStats()
  if (response.error || !response.data) {
    throw new Error(
      problemMessage(response.error, 'Failed to fetch email event stats')
    )
  }
  return response.data
}
