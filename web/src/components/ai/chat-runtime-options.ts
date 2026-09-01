// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export interface ChatSelectOption {
  id: string
  name: string
  description?: string | null
}

export interface ChatModelOption extends ChatSelectOption {
  thinking_options: ChatSelectOption[]
  tool_thinking_options?: ChatSelectOption[] | null
  default_thinking_option_id?: string | null
}

export interface ChatProviderOption extends ChatSelectOption {
  auth_source: string
  models: ChatModelOption[]
  default_model_id?: string | null
  model_discovery_status?: string
  model_discovery_error?: string | null
  permission_modes: ChatSelectOption[]
  default_permission_mode_id?: string | null
}

export interface ChatRuntimeSelection {
  providerId: string
  modelId: string | null
  thinkingOptionId: string | null
  permissionModeId: string | null
}

function firstValidId(
  options: ChatSelectOption[],
  preferred?: string | null,
  fallback?: string | null
): string | null {
  for (const candidate of [preferred, fallback]) {
    if (candidate && options.some((option) => option.id === candidate)) {
      return candidate
    }
  }
  return options[0]?.id ?? null
}

/**
 * Resolve a complete, provider-valid runtime selection. Provider changes call
 * this without the previous model/mode values, while conversation restoration
 * supplies every persisted value. In both cases the result can only contain
 * options advertised by the selected provider and model.
 */
export function resolveChatRuntimeSelection(
  providers: ChatProviderOption[],
  providerId: string,
  preferred?: Partial<Omit<ChatRuntimeSelection, 'providerId'>>
): ChatRuntimeSelection {
  const provider =
    providers.find((option) => option.id === providerId) ?? providers[0]
  if (!provider) {
    return {
      providerId,
      modelId: null,
      thinkingOptionId: null,
      permissionModeId: null,
    }
  }

  const modelId = firstValidId(
    provider.models,
    preferred?.modelId,
    provider.default_model_id
  )
  const model = provider.models.find((option) => option.id === modelId)
  // Project chat always sends function tools. Prefer a model's narrower tool
  // compatibility when it differs from its general reasoning capabilities.
  const thinkingOptions =
    model?.tool_thinking_options ?? model?.thinking_options ?? []
  const thinkingOptionId = firstValidId(
    thinkingOptions,
    preferred?.thinkingOptionId,
    model?.default_thinking_option_id
  )
  const permissionModeId = firstValidId(
    provider.permission_modes,
    preferred?.permissionModeId,
    provider.default_permission_mode_id
  )

  return {
    providerId: provider.id,
    modelId,
    thinkingOptionId,
    permissionModeId,
  }
}

/**
 * Keep the pinned harness while reconciling model capabilities returned by a
 * forced refresh. Stale model/thinking values are dropped, but a temporarily
 * absent provider never makes an existing conversation switch harnesses.
 */
export function reconcileChatRuntimeAfterRefresh(
  providers: ChatProviderOption[],
  current: ChatRuntimeSelection
): ChatRuntimeSelection {
  if (!providers.some((provider) => provider.id === current.providerId)) {
    return current
  }
  return resolveChatRuntimeSelection(providers, current.providerId, current)
}

export function chatProviderLabel(provider: ChatProviderOption): string {
  const source =
    provider.auth_source === 'host_environment'
      ? 'Host environment'
      : 'configured key'
  return `${provider.name} · ${source}`
}
