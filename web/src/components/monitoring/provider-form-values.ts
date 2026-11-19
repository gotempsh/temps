// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useRef } from 'react'
import type { UseFormReturn } from 'react-hook-form'
import type { NotificationProviderResponse } from '@/api/client/types.gen'
import { type ProviderFormData, telegramFormFieldsFromConfig } from './schemas'

/** The edit form's values for a stored provider. */
export function providerFormValues(
  provider: NotificationProviderResponse
): ProviderFormData {
  const config = (provider.config ?? {}) as Record<string, unknown>
  return {
    name: provider.name,
    provider_type: provider.provider_type as ProviderFormData['provider_type'],
    config: {
      // Slack config
      webhook_url: config.webhook_url as string,
      channel: config.channel as string,

      // Cloudflare config
      account_id: config.account_id as string,
      api_token: config.api_token as string,

      // Telegram config
      ...telegramFormFieldsFromConfig(config),

      // Email config
      smtp_host: config.smtp_host as string,
      smtp_port: config.smtp_port as number,
      use_credentials: !!(config.username || config.password),
      smtp_username: config.username as string,
      password: config.password as string,
      from_name: config.from_name as string,
      from_address: config.from_address as string,
      to_addresses: config.to_addresses as string[],
      tls_mode: (config.tls_mode as 'None' | 'Starttls' | 'Tls') || undefined,
      starttls_required: config.starttls_required as boolean,
      accept_invalid_certs: config.accept_invalid_certs as boolean,

      // Webhook config
      url: config.url as string,
      method: (config.method as 'POST' | 'PUT' | 'PATCH') || 'POST',
      headers: (config.headers as Record<string, string>) || {},
      timeout_secs: (config.timeout_secs as number) || 30,
    },
  }
}

/**
 * Loads `provider` into `form` the first time it arrives and whenever the page
 * switches to a different provider. A refetch of the same provider (reconnect,
 * invalidation) does not reset the form, so unsaved edits survive it.
 */
export function useLoadProviderIntoForm(
  provider: NotificationProviderResponse | undefined,
  form: Pick<UseFormReturn<ProviderFormData>, 'reset'>
) {
  const loadedProviderId = useRef<number | null>(null)
  useEffect(() => {
    if (!provider || loadedProviderId.current === provider.id) return
    loadedProviderId.current = provider.id
    form.reset(providerFormValues(provider))
  }, [provider, form])
}
