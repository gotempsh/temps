// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { z } from 'zod'
import type { SubscriptionSettingsResponse } from '@/api/client'

/** One thing standing between the page and email subscriptions. */
export interface SubscriptionBlocker {
  reason: string
  /** Console path that fixes it; `null` when the fix is on this form. */
  setupPath: string | null
}

/**
 * What still has to be set up before subscriptions can be switched on, in
 * the order the server checks them. Empty when they can.
 */
export function subscriptionBlockers(
  settings: SubscriptionSettingsResponse
): SubscriptionBlocker[] {
  const blockers: SubscriptionBlocker[] = []
  for (const capability of [settings.email, settings.links]) {
    if (!capability.configured) {
      blockers.push({
        reason: capability.reason ?? 'Not configured',
        setupPath: capability.setup_path ?? null,
      })
    }
  }
  return blockers
}

/** Whether the sender's domain is one mail can go out from. */
export function senderDomainProblem(
  sender: string,
  verifiedDomains: string[]
): string | null {
  const domain = sender.trim().toLowerCase().split('@')[1] ?? ''
  if (!sender.trim() || verifiedDomains.length === 0) return null
  return verifiedDomains.includes(domain)
    ? null
    : `Use an address at ${verifiedDomains.join(', ')}`
}

export const subscriptionSettingsSchema = z.object({
  sender: z
    .string()
    .trim()
    .max(254, 'At most 254 characters')
    .refine(
      (value) => value === '' || z.string().email().safeParse(value).success,
      'Enter an email address'
    ),
})
export type SubscriptionSettingsValues = z.infer<
  typeof subscriptionSettingsSchema
>

/** The body of a settings save. An empty sender clears it. */
export function settingsRequest(
  values: SubscriptionSettingsValues,
  enabled: boolean
) {
  return { enabled, sender: values.sender.trim() }
}

/**
 * The request the on/off switch sends. Switching off sends only the switch,
 * so an unsaved or invalid sender edit never blocks turning mail off.
 */
export function switchRequest(
  next: boolean,
  values: SubscriptionSettingsValues
): { enabled: boolean; sender?: string } {
  return next ? settingsRequest(values, true) : { enabled: false }
}

/**
 * The list page to show after deleting one subscriber, so deleting the last
 * row of the last page moves back instead of leaving an empty page.
 */
export function pageAfterRemoval(
  page: number,
  totalBefore: number,
  pageSize: number
) {
  const pages = Math.max(1, Math.ceil(Math.max(0, totalBefore - 1) / pageSize))
  return Math.min(page, pages)
}

/**
 * The sender the form should show after the stored settings change, or
 * `null` to leave the field alone: a refetch never replaces what the user
 * is typing.
 */
export function senderAfterRefetch(
  stored: string | null | undefined,
  hasUnsavedEdits: boolean
): string | null {
  return hasUnsavedEdits ? null : (stored ?? '')
}

export const SUBSCRIBERS_PAGE_SIZE = 20
