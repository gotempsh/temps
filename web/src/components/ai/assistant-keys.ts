// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { aiProviderName, providerKeyEndpoint } from '@/lib/ai-providers'
import {
  type ChatProviderOption,
  type ChatRuntimeSelection,
  resolveChatRuntimeSelection,
} from './chat-runtime-options'

/** The parts of an AI provider key the assistant's provider choice reads. */
export interface AssistantKey {
  id: number
  provider: string
  display_name: string
  is_active: boolean
  base_url?: string | null
}

/** Prefix of a chat provider id that runs on one AI gateway key. */
const GATEWAY_KEY_PREFIX = 'gateway_key:'

/** The keys an assistant chat can run on, in the order the API lists them. */
export function activeAssistantKeys<K extends AssistantKey>(
  keys: readonly K[]
): K[] {
  return keys.filter((key) => key.is_active)
}

/**
 * The key a new assistant chat runs on when the user picks none: the active
 * OpenRouter key if there is one, else the first active key. The console
 * sends the chosen key with the new chat, so the chat runs on the key whose
 * models the picker listed.
 */
export function defaultAssistantKey<K extends AssistantKey>(
  keys: readonly K[]
): K | null {
  const active = activeAssistantKeys(keys)
  return (
    active.find((key) => key.provider === 'openrouter') ?? active[0] ?? null
  )
}

/** The preferred key while it is still active, else the default key. */
export function resolveAssistantKey<K extends AssistantKey>(
  keys: readonly K[],
  preferredId: number | null
): K | null {
  const preferred = activeAssistantKeys(keys).find(
    (key) => key.id === preferredId
  )
  return preferred ?? defaultAssistantKey(keys)
}

/** The chat provider id that runs a conversation on one gateway key. */
export function gatewayKeyProviderId(keyId: number): string {
  return `${GATEWAY_KEY_PREFIX}${keyId}`
}

/** The gateway key a chat provider id runs on, or null for anything else. */
export function gatewayKeyIdFromProvider(
  providerId: string | null | undefined
): number | null {
  if (!providerId?.startsWith(GATEWAY_KEY_PREFIX)) return null
  const id = Number(providerId.slice(GATEWAY_KEY_PREFIX.length))
  return Number.isInteger(id) && id > 0 ? id : null
}

/**
 * How a key is named in the assistant's provider choice: its display name,
 * plus the endpoint host when it has a custom base URL, since two
 * OpenAI-compatible keys are told apart by where they point.
 */
export function assistantKeyLabel(key: AssistantKey): string {
  const name = key.display_name.trim() || aiProviderName(key.provider)
  const endpoint = providerKeyEndpoint(key.base_url)
  return endpoint ? `${name} · ${endpoint}` : name
}

/**
 * The runtime an assistant chat moves to when the user picks `modelId` on
 * `providerId` (a gateway key). A key the provider status has not listed yet
 * is still taken as chosen, with no reasoning override: the server validates
 * the model against that key when the next message is sent, so the choice is
 * never silently dropped or moved to another key.
 */
export function assistantRuntimeSelection(
  providers: ChatProviderOption[],
  current: ChatRuntimeSelection,
  providerId: string,
  modelId: string
): ChatRuntimeSelection {
  if (!providers.some((provider) => provider.id === providerId)) {
    return {
      providerId,
      modelId,
      thinkingOptionId: null,
      permissionModeId: current.permissionModeId,
    }
  }
  return resolveChatRuntimeSelection(providers, providerId, {
    modelId,
    permissionModeId: current.permissionModeId,
  })
}
