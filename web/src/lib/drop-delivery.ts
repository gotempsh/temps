// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { CloudflareProjectCapability } from '@/api/client'

export type DropDeliveryChoice = 'none' | 'cloudflare' | 'bunny'

export const DROP_DELIVERY_LABELS: Record<DropDeliveryChoice, string> = {
  none: 'No CDN',
  cloudflare: 'Cloudflare',
  bunny: 'bunny.net',
}

/**
 * Providers a dropped project can be created with. Only providers the operator
 * has already set up are offered: drop is a one-shot flow, so it never asks the
 * user to configure a CDN mid-upload.
 */
export function dropDeliveryChoices(
  capability: CloudflareProjectCapability | undefined
): DropDeliveryChoice[] {
  const choices: DropDeliveryChoice[] = ['none']
  if (capability?.configured || capability?.default_enabled)
    choices.push('cloudflare')
  if (capability?.bunny_configured || capability?.bunny_default_enabled)
    choices.push('bunny')
  return choices
}
