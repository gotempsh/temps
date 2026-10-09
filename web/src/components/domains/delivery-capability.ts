// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  getCloudflareProjectCapability,
  type CloudflareProjectCapability,
} from '@/api/client'
import { useQuery } from '@tanstack/react-query'
import type { DeliveryProviderChoiceValue } from './DeliveryProviderChoice'

export function useDeliveryProjectCapability() {
  return useQuery({
    queryKey: ['cloudflare-project-capability'],
    queryFn: async () => {
      const response = await getCloudflareProjectCapability()
      if (response.error || !response.data)
        throw new Error('Could not load Cloudflare availability')
      return response.data
    },
  })
}

/** The provider the server applies to a new project when no override is sent. */
export function defaultDeliveryChoice(
  capability: CloudflareProjectCapability | undefined
): DeliveryProviderChoiceValue {
  if (capability?.default_enabled) return 'cloudflare'
  if (capability?.bunny_default_enabled) return 'bunny'
  return 'none'
}
