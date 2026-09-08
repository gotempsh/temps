// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ProviderCatalogDto } from '@/api/client'

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
  model_source?: ProviderCatalogDto['model_source']
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

export type ChatHarnessCatalogOption = Pick<
  ProviderCatalogDto,
  | 'id'
  | 'name'
  | 'workspace_ready'
  | 'runtime_models'
  | 'default_runtime_model_id'
  | 'model_source'
  | 'permission_modes'
  | 'default_permission_mode_id'
>

/** Host CLI threads use the Agent Sandbox catalog, regardless of scope. */
export function usesHarnessCatalog(contextType: string): boolean {
  return contextType === 'application' || contextType === 'global'
}

/**
 * A saved workspace credential with no authoritative catalog needs one
 * automatic discovery attempt. The backend resolves that attempt through the
 * user's persistent workspace, creating or waking its sandbox as needed.
 */
export function shouldAutoRefreshHarnessModels(
  contextType: string,
  provider: ChatProviderOption | undefined,
  alreadyAttempted: boolean
): boolean {
  return (
    usesHarnessCatalog(contextType) &&
    !alreadyAttempted &&
    provider !== undefined &&
    provider.id !== 'gateway' &&
    providerCatalogNeedsRefresh(provider)
  )
}

/** Bootstrap and stale catalogs are useful continuity data, not proof that
 * the saved workspace credential can use those models. */
export function providerCatalogNeedsRefresh(
  provider: ChatProviderOption
): boolean {
  return (
    provider.model_source === 'bootstrap' ||
    provider.model_source === 'stale_cache' ||
    provider.model_discovery_status === 'unavailable'
  )
}

/** Convert the Agent Sandbox catalog into the provider-neutral chat shape. */
export function chatHarnessProviderOptions(
  providers: ChatHarnessCatalogOption[]
): ChatProviderOption[] {
  return providers
    .filter((provider) => provider.workspace_ready)
    .map((provider) => {
      const models = (provider.runtime_models ?? []).map((model) => ({
        id: model.id,
        name: model.name,
        thinking_options: model.thinking_modes,
        tool_thinking_options: model.tool_thinking_modes,
        default_thinking_option_id: model.default_thinking_mode_id,
      }))
      const catalogIsAuthoritative =
        provider.model_source !== 'bootstrap' &&
        provider.model_source !== 'stale_cache'
      const modelDiscoveryReady = catalogIsAuthoritative && models.length > 0
      return {
        id: provider.id,
        name: provider.name,
        auth_source: 'host_environment',
        models,
        default_model_id: provider.default_runtime_model_id,
        model_source: provider.model_source,
        model_discovery_status: modelDiscoveryReady ? 'ready' : 'unavailable',
        model_discovery_error: modelDiscoveryReady
          ? null
          : `Could not resolve current models for ${provider.name}.`,
        permission_modes: provider.permission_modes ?? [],
        default_permission_mode_id: provider.default_permission_mode_id,
      }
    })
}

/**
 * Isolate drafts by durable conversation identity. Before the first message
 * creates a conversation, retain a context-scoped key so lazy-create drafts
 * still survive a reload.
 */
export function chatDraftStorageKey({
  userScoped,
  projectId,
  contextType,
  contextId,
  conversationPublicId,
}: {
  userScoped: boolean
  projectId?: number
  contextType: string
  contextId: string
  conversationPublicId?: string | null
}): string {
  const owner = userScoped ? 'user' : String(projectId)
  const target = conversationPublicId
    ? `conversation:${conversationPublicId}`
    : `context:${contextType}:${contextId}`
  return `temps.ai.draft.${owner}:${target}`
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
 * supplies every persisted value. Live/cached account inventories validate
 * those values; bootstrap or stale inventories preserve an unresolved saved
 * model until an authoritative refresh can decide whether it is valid.
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

  const modelCatalogIsAuthoritative =
    provider.model_source !== 'bootstrap' &&
    provider.model_source !== 'stale_cache'
  const preferredModelIsUnresolved =
    !modelCatalogIsAuthoritative &&
    Boolean(preferred?.modelId) &&
    !provider.models.some((model) => model.id === preferred?.modelId)
  const modelId = preferredModelIsUnresolved
    ? (preferred?.modelId ?? null)
    : firstValidId(
        provider.models,
        preferred?.modelId,
        provider.default_model_id
      )
  const model = provider.models.find((option) => option.id === modelId)
  // Project chat always sends function tools. Prefer a model's narrower tool
  // compatibility when it differs from its general reasoning capabilities.
  const thinkingOptions =
    model?.tool_thinking_options ?? model?.thinking_options ?? []
  const preserveUnverifiedThinking =
    !modelCatalogIsAuthoritative &&
    preferred !== undefined &&
    Object.prototype.hasOwnProperty.call(preferred, 'thinkingOptionId')
  const thinkingOptionId = preserveUnverifiedThinking
    ? (preferred?.thinkingOptionId ?? null)
    : firstValidId(
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
 * forced refresh. Authoritatively stale model/thinking values are dropped,
 * but an incomplete catalog or temporarily absent provider never rewrites an
 * existing conversation's runtime selection.
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

/** Keep provider protocol sentinels out of the user-facing runtime controls. */
export function chatModelLabel(
  provider: ChatProviderOption,
  model: ChatModelOption
): string {
  const name = model.name.trim()
  if (model.id === 'default' && name.toLowerCase() === 'default') {
    return `${provider.name} model`
  }
  return name.replace(/^default\s*·\s*/i, '')
}

export function chatThinkingLabel(option: ChatSelectOption): string {
  return option.id === 'default' ? 'Auto' : option.name
}

/** Radix copies a selected item's content into SelectValue. Keep that content
 * text-only: the trigger owns the single thinking icon. */
export function chatThinkingItemContent(option: ChatSelectOption): string {
  return chatThinkingLabel(option)
}

export function chatPermissionLabel(option: ChatSelectOption): string {
  switch (option.id) {
    case 'auto':
    case 'full-access':
      return 'Auto'
    case 'default':
      return 'Ask each time'
    default:
      return option.name
  }
}
