// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { getCloudflareProjectCapability } from '@/api/client'
import { useQuery } from '@tanstack/react-query'
import { Link } from 'react-router'
import {
  DeliveryProviderChoice,
  type DeliveryProviderChoiceValue,
} from './DeliveryProviderChoice'

/** An omitted override lets the server apply the current future-project default. */
export function DeliveryProjectOption({
  value,
  onChange,
}: {
  value: DeliveryProviderChoiceValue | undefined
  onChange: (value: DeliveryProviderChoiceValue) => void
}) {
  const capability = useQuery({
    queryKey: ['cloudflare-project-capability'],
    queryFn: async () => {
      const response = await getCloudflareProjectCapability()
      if (response.error || !response.data)
        throw new Error('Could not load Cloudflare availability')
      return response.data
    },
  })
  const selected =
    value ??
    (capability.data?.default_enabled
      ? 'cloudflare'
      : capability.data?.bunny_default_enabled
        ? 'bunny'
        : 'none')
  return (
    <div className="space-y-3">
      <div>
        <p className="font-medium">Delivery provider</p>
        <p className="text-sm text-muted-foreground">
          Choose the default for this project. You can change it later in
          Domains; existing domain bindings keep their applied configuration.
        </p>
        {!capability.data?.configured && capability.data?.reason && (
          <p className="text-sm text-muted-foreground">
            {capability.data.reason}{' '}
            <Link
              to={capability.data.setup_path ?? '/delivery-profiles'}
              className="underline"
            >
              Set up Cloudflare
            </Link>
          </p>
        )}
        {!capability.data?.bunny_configured &&
          capability.data?.bunny_reason && (
            <p className="text-sm text-muted-foreground">
              {capability.data.bunny_reason}{' '}
              <Link to="/delivery-profiles" className="underline">
                Set up bunny.net
              </Link>
            </p>
          )}
        {capability.isError && (
          <p className="text-sm text-destructive">
            Could not load delivery availability.
          </p>
        )}
      </div>
      <DeliveryProviderChoice
        value={selected}
        onChange={onChange}
        cloudflareConfigured={capability.data?.configured ?? false}
        bunnyConfigured={capability.data?.bunny_configured ?? false}
        disabled={capability.isPending || capability.isError}
      />
    </div>
  )
}
