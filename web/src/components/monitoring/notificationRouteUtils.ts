// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { NotificationProviderResponse } from '@/api/client/types.gen'

export const severityLabels: Record<string, string> = {
  debug: 'Debug',
  info: 'Info',
  warning: 'Warning',
  error: 'Error',
  critical: 'Critical',
  emergency: 'Emergency',
}

export const severities = Object.keys(severityLabels)

export const severityRangeLabel = (minimum: string, maximum: string) => {
  if (minimum === 'debug' && maximum === 'emergency') return 'All severities'
  if (minimum === maximum) return `${severityLabels[minimum]} only`
  if (maximum === 'emergency') return `${severityLabels[minimum]} and above`
  return `${severityLabels[minimum]} through ${severityLabels[maximum]}`
}

export const configuredSlackChannel = (
  provider: NotificationProviderResponse
) => {
  if (
    provider.provider_type !== 'slack' ||
    !provider.config ||
    typeof provider.config !== 'object'
  ) {
    return undefined
  }
  const channel = (provider.config as { channel?: unknown }).channel
  return typeof channel === 'string' && channel.trim()
    ? channel.trim()
    : undefined
}
