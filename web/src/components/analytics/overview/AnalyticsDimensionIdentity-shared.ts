// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { getReferrerDisplayName } from './ReferrerIdentity'
import { getLanguageName } from '@/lib/analytics-language'

export const regions = new Intl.DisplayNames(['en'], { type: 'region' })

export const countryCodes = new Map<string, string>()

for (let a = 65; a <= 90; a++) {
  for (let b = 65; b <= 90; b++) {
    const code = String.fromCharCode(a, b)
    const name = regions.of(code)
    if (name && name !== code) countryCodes.set(name.toLowerCase(), code)
  }
}

export function countryFlag(value: string): string | undefined {
  const code = /^[a-z]{2}$/i.test(value)
    ? value.toUpperCase()
    : countryCodes.get(value.toLowerCase())
  if (!code || regions.of(code) === code) return undefined
  return [...code]
    .map((c) => String.fromCodePoint(c.charCodeAt(0) + 127397))
    .join('')
}

export function dimensionLabel(
  dimension: string | undefined,
  value: string
): string {
  if (dimension === 'referrer_hostname') return getReferrerDisplayName(value)
  if (dimension === 'language') return getLanguageName(value)
  if (dimension === 'country' && /^[a-z]{2}$/i.test(value))
    return regions.of(value.toUpperCase()) ?? value
  return value || 'Unknown'
}
