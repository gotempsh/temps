// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { TestProviderKeyRequest } from '@/api/client'
import {
  AI_PROVIDERS,
  isSelfHostedProvider,
  type AiProviderId,
  type AiProviderMeta,
} from '@/lib/ai-providers-shared'
import type { ProviderKeyFormValues } from './provider-key-form'

/**
 * The order the choices are offered in: the one key that reaches every
 * vendor's models first, then each vendor, then a server of the user's own.
 */
const CHOICE_ORDER: readonly AiProviderId[] = [
  'openrouter',
  'openai',
  'anthropic',
  'gemini',
  'xai',
  'openai_compatible',
]

/** Every provider the assistant can run on, in the order they are offered. */
export const ASSISTANT_PROVIDER_CHOICES: readonly AiProviderMeta[] =
  CHOICE_ORDER.flatMap((id) => AI_PROVIDERS.filter((meta) => meta.id === id))

/** How each vendor's keys begin, so a pasted key can be checked by eye. */
export const PROVIDER_KEY_PLACEHOLDER: Record<AiProviderId, string> = {
  openrouter: 'sk-or-…',
  openai: 'sk-…',
  anthropic: 'sk-ant-…',
  gemini: 'AIza…',
  xai: 'xai-…',
  openai_compatible: '',
}

/** What the form holds that a test or a save is made from. */
type ProviderInputs = Pick<
  ProviderKeyFormValues,
  | 'provider'
  | 'api_key'
  | 'base_url'
  | 'default_model'
  | 'trust_private_address'
>

/**
 * Request body for `POST /ai/providers/test`: the same inputs a save would
 * send, tried once without storing anything. A vendor is tested on a small
 * model of its own; a server of the user's own on the model they picked.
 */
export function providerTestRequest(
  values: ProviderInputs
): TestProviderKeyRequest {
  if (!isSelfHostedProvider(values.provider)) {
    return { provider: values.provider, api_key: values.api_key }
  }
  return {
    provider: values.provider,
    api_key: values.api_key,
    base_url: values.base_url.trim(),
    default_model: values.default_model,
    trust_private_address: values.trust_private_address,
  }
}

/**
 * Identifies the inputs a test was made with. A result is only shown while
 * they are unchanged, so "it works" never stands under an edited key.
 */
export function providerTestSignature(values: ProviderInputs): string {
  return JSON.stringify(providerTestRequest(values))
}
