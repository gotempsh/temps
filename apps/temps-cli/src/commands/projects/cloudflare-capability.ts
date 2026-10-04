// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { getCloudflareProjectCapability } from '../../api/sdk.gen.js'
import type { CloudflareProjectCapability } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import {
  newline,
  header,
  icons,
  json,
  colors,
  info,
  keyValue,
} from '../../ui/output.js'

export const DELIVERY_PROVIDER_CHOICES = [
  'none',
  'cloudflare',
  'bunny',
] as const
export type DeliveryProviderChoice = (typeof DELIVERY_PROVIDER_CHOICES)[number]

/** Validate `projects create --delivery-provider` against the values the API accepts. */
export function parseDeliveryProviderChoice(
  value: string
): DeliveryProviderChoice | undefined {
  const normalized = value.trim().toLowerCase()
  return DELIVERY_PROVIDER_CHOICES.find((choice) => choice === normalized)
}

/**
 * The delivery provider a new project gets when `--delivery-provider` is
 * omitted, mirroring the server: Cloudflare's default wins over Bunny's.
 */
export function defaultDeliveryProvider(
  capability: CloudflareProjectCapability
): DeliveryProviderChoice {
  if (capability.default_enabled) return 'cloudflare'
  if (capability.bunny_default_enabled) return 'bunny'
  return 'none'
}

function readiness(
  configured: boolean,
  reason: string | null | undefined
): string {
  if (configured) return colors.success('configured')
  return colors.warning(reason ? `not configured: ${reason}` : 'not configured')
}

export async function cloudflareCapabilityAction(options: {
  json?: boolean
}): Promise<void> {
  await requireAuth()
  await setupClient()

  const capability = await withSpinner(
    'Fetching delivery capability...',
    async () => {
      const { data, error } = await getCloudflareProjectCapability({ client })
      if (error || !data) {
        throw new Error(
          getErrorMessage(error) ||
            'Failed to fetch the project delivery capability'
        )
      }
      return data
    }
  )

  if (options.json) {
    json(capability)
    return
  }

  newline()
  header(`${icons.info} Delivery for new projects`)
  keyValue('Cloudflare', readiness(capability.configured, capability.reason))
  keyValue(
    'Cloudflare on by default',
    capability.default_enabled ? 'yes' : 'no'
  )
  keyValue(
    'Bunny',
    readiness(capability.bunny_configured, capability.bunny_reason)
  )
  keyValue(
    'Bunny on by default',
    capability.bunny_default_enabled ? 'yes' : 'no'
  )
  keyValue('New project default', defaultDeliveryProvider(capability))
  if (capability.setup_path && !capability.configured) {
    keyValue('Cloudflare setup', `${capability.setup_path} in the console`)
  }
  if (!capability.bunny_configured) {
    keyValue(
      'Bunny setup',
      'temps delivery-profiles create --kind bunny --name bunny --pull-zone-id <id> --api-key-stdin'
    )
  }
  newline()
  info(
    'Override per project: temps projects create --delivery-provider <none|cloudflare|bunny>'
  )
  newline()
}
