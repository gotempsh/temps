// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { updateProviderKey } from '@/api/client'
import {
  getAiProviderStatusOptions,
  getAiProviderStatusQueryKey,
  getProviderKeyOptions,
  getProviderKeyQueryKey,
  listProviderKeysOptions,
  listProviderKeysQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useCallback, useMemo, useState } from 'react'
import {
  assistantModelOptions,
  imageSupport,
  type ImageSupport,
} from './assistant-models'
import {
  activeAssistantKeys,
  defaultAssistantKey,
  resolveAssistantKey,
} from './assistant-keys'
import {
  loadLastAssistantKeyId,
  loadLastAssistantModel,
  saveLastAssistantKeyId,
  saveLastAssistantModel,
} from './assistant-state'

/**
 * The gateway keys an assistant chat can run on, and the one it runs on by
 * default: the active OpenRouter key if there is one, else the first active
 * key.
 */
export function useAssistantProviderKeys(enabled = true) {
  const query = useQuery({
    ...listProviderKeysOptions(),
    staleTime: 15_000,
    retry: false,
    enabled,
  })
  const keys = useMemo(
    () => activeAssistantKeys(query.data ?? []),
    [query.data]
  )
  return {
    keys,
    defaultKey: defaultAssistantKey(keys),
    isPending: query.isPending,
  }
}

/** The gateway key the assistant runs on by default. */
export function useAssistantProviderKey(enabled = true) {
  const { defaultKey, isPending } = useAssistantProviderKeys(enabled)
  return { key: defaultKey, isPending }
}

/** The key's models with what the provider's catalogue says about each. */
export function useProviderModels(providerKeyId: number | null) {
  const detail = useQuery({
    ...getProviderKeyOptions({ path: { id: providerKeyId ?? 0 } }),
    enabled: providerKeyId !== null,
    staleTime: 60_000,
    retry: false,
  })
  const options = useMemo(
    () => assistantModelOptions(detail.data?.models),
    [detail.data?.models]
  )
  return { ...detail, options }
}

/**
 * Whether the model a chat runs on reads images, per the provider catalogue.
 * Only queries when `enabled` (an assistant chat); other chats never need it.
 */
export function useAssistantImageSupport(
  enabled: boolean,
  modelId: string | null,
  /** The key the chat runs on; the default key when not known. */
  providerKeyId: number | null = null
): ImageSupport {
  const { key } = useAssistantProviderKey(enabled)
  const keyId = providerKeyId ?? key?.id ?? null
  const models = useProviderModels(enabled ? keyId : null)
  return useMemo(() => {
    if (!enabled || !modelId) return 'unknown'
    return imageSupport(models.options.find((m) => m.id === modelId)?.details)
  }, [enabled, modelId, models.options])
}

/** Persist the model new assistant chats start on for this key. */
export function useSetDefaultModel(providerKeyId: number | null) {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: async (modelId: string) => {
      if (providerKeyId === null) throw new Error('No AI provider key')
      return updateProviderKey({
        path: { id: providerKeyId },
        body: { default_model: modelId },
        throwOnError: true,
      })
    },
    onSuccess: async () => {
      await Promise.all([
        queryClient.invalidateQueries({
          queryKey: getProviderKeyQueryKey({
            path: { id: providerKeyId ?? 0 },
          }),
        }),
        queryClient.invalidateQueries({ queryKey: listProviderKeysQueryKey() }),
        queryClient.invalidateQueries({
          queryKey: getAiProviderStatusQueryKey(),
        }),
      ])
    },
  })
}

/**
 * The model a new chat starts on when none is picked: the key's pinned default,
 * else whatever the server will actually resolve (the provider status reports
 * it per key). Never a placeholder; null only while that is still loading.
 */
export function useEffectiveDefaultModel(
  key: { id: number; default_model?: string | null } | null
): string | null {
  const status = useQuery({
    ...getAiProviderStatusOptions(),
    staleTime: 15_000,
    retry: false,
    enabled: key !== null,
  })
  if (key === null) return null
  if (key.default_model) return key.default_model
  const provider = status.data?.available_providers?.find(
    (candidate) => candidate.id === `gateway_key:${key.id}`
  )
  return provider?.default_model_id ?? null
}

/**
 * The provider key and model a new chat will be created with. Chosen per
 * chat: picking one changes only this chat. It starts on the key and model
 * last used when they are still offered, else the default key and its default
 * model, so the user is not asked again every time.
 */
export function useNewChatModel() {
  const { keys } = useAssistantProviderKeys()
  const [picked, setPicked] = useState<{
    keyId: number
    model: string
  } | null>(null)
  const [lastKeyId] = useState(loadLastAssistantKeyId)
  const [lastModel] = useState(loadLastAssistantModel)

  const key = resolveAssistantKey(keys, picked?.keyId ?? lastKeyId)
  const models = useProviderModels(key?.id ?? null)
  const defaultModel = useEffectiveDefaultModel(key)

  const offered = (modelId: string | null) =>
    modelId !== null && models.options.some((option) => option.id === modelId)
  const model =
    picked && picked.keyId === key?.id
      ? picked.model
      : offered(lastModel)
        ? lastModel
        : defaultModel

  const choose = useCallback((modelId: string, keyId: number) => {
    setPicked({ keyId, model: modelId })
    saveLastAssistantModel(modelId)
    saveLastAssistantKeyId(keyId)
  }, [])
  return { keyId: key?.id ?? null, model, choose }
}
