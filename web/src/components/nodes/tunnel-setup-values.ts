// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// What the worker's tunnel setup form sends: a managed domain picked with an
// optional label becomes the tunnel's domain and, one level deep, its
// hostname suffix.

import type { ProviderInfo } from '@/api/client/types.gen'

/**
 * How a label picked with a zone names apps: `app-lab.example.com`, one
 * level deep (the tunnel's `host_suffix`, the tunnel serving the zone), or
 * `app.lab.example.com` under the subdomain `lab.example.com`.
 */
export type LabelLayout = 'suffix' | 'subdomain'

/** A managed domain as picked: a zone and an optional label for it. */
export interface DomainPick {
  zone: string
  subdomain: string
  layout: LabelLayout
}

/** One level deep by default: Cloudflare's free certificate covers it. */
export const EMPTY_PICK: DomainPick = {
  zone: '',
  subdomain: '',
  layout: 'suffix',
}
/** The form's answers, as far as the request is concerned. */
export interface SetupAnswers {
  values: Record<string, string>
  picks: Record<string, DomainPick>
}

export function subdomainLabel(subdomain: string) {
  return subdomain.trim().toLowerCase().replace(/\.+$/, '')
}

/**
 * The values the API takes: a picked domain becomes one hostname (the zone
 * when its label is a suffix).
 */
export function providerValues(provider: ProviderInfo, answers: SetupAnswers) {
  const values: Record<string, string> = {}
  for (const [key, value] of Object.entries(answers.values)) {
    if (value.trim()) values[key] = value
  }
  for (const field of provider.fields) {
    if (field.kind.type !== 'managed_domain') continue
    const pick = answers.picks[field.key] ?? EMPTY_PICK
    const label = subdomainLabel(pick.subdomain)
    const domain =
      pick.zone && label && pick.layout === 'subdomain'
        ? `${label}.${pick.zone}`
        : pick.zone
    if (domain) values[field.key] = domain
  }
  return values
}

/** The tunnel's `host_suffix`: a label picked to stay one level deep. */
export function providerHostSuffix(
  provider: ProviderInfo,
  answers: SetupAnswers
): string | null {
  for (const field of provider.fields) {
    if (field.kind.type !== 'managed_domain') continue
    const pick = answers.picks[field.key] ?? EMPTY_PICK
    const label = subdomainLabel(pick.subdomain)
    if (pick.zone && label && pick.layout === 'suffix') return label
  }
  return null
}
