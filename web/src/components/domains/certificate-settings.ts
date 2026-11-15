// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { DomainCertificateSettings } from '@/api/client'

export function certificateSettingsErrors(
  settings: DomainCertificateSettings,
  wildcard: boolean
): Record<string, string | undefined> {
  const validDays = (value: number) =>
    Number.isInteger(value) && value >= 1 && value <= 90
  return {
    'Validation method': !['http-01', 'dns-01'].includes(
      settings.challenge_type
    )
      ? 'Choose a validation method.'
      : wildcard && settings.challenge_type !== 'dns-01'
        ? 'Wildcard certificates require DNS-01.'
        : undefined,
    'Renewal window': validDays(settings.renewal_days)
      ? undefined
      : 'Enter a whole number from 1 to 90.',
    'Alert window':
      settings.alert_days == null || validDays(settings.alert_days)
        ? undefined
        : 'Enter a whole number from 1 to 90, or leave blank.',
  }
}
