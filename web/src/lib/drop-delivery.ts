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
 * Providers a dropped project can be created with. Only providers that are
 * ready are offered: the server rejects an explicit choice of a provider that
 * is not set up, and drop is a one-shot flow that should not fail on it.
 */
export function dropDeliveryChoices(
  capability: CloudflareProjectCapability | undefined
): DropDeliveryChoice[] {
  const choices: DropDeliveryChoice[] = ['none']
  if (capability?.configured) choices.push('cloudflare')
  if (capability?.bunny_configured) choices.push('bunny')
  return choices
}

/**
 * The delivery the server applies when the drop sends no override. Mirrors
 * project creation: Cloudflare's default wins over Bunny's, and a default
 * whose provider is not ready creates the project without CDN delivery.
 */
export function effectiveDropDeliveryDefault(
  capability: CloudflareProjectCapability | undefined
): DropDeliveryChoice {
  if (capability?.default_enabled)
    return capability.configured ? 'cloudflare' : 'none'
  if (capability?.bunny_default_enabled)
    return capability.bunny_configured ? 'bunny' : 'none'
  return 'none'
}
