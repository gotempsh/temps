// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  ProviderCatalogResponse,
  RefreshProviderModelsResponse,
} from '@/api/client'

export function isSavedProviderModelUnavailable({
  savedModel,
  availableModels,
  source,
}: {
  savedModel: string
  availableModels: string[]
  source: string
}): boolean {
  if (savedModel === '' || (source !== 'live' && source !== 'cache')) {
    return false
  }
  return !availableModels.includes(savedModel)
}

export function mergeProviderModelRefresh(
  catalog: ProviderCatalogResponse,
  refresh: RefreshProviderModelsResponse
): ProviderCatalogResponse {
  return {
    ...catalog,
    providers: catalog.providers.map((provider) =>
      provider.id === refresh.provider_id
        ? {
            ...provider,
            models: refresh.runtime_models.map((model) => model.id),
            runtime_models: refresh.runtime_models,
            default_runtime_model_id: refresh.default_runtime_model_id ?? null,
            model_source: refresh.model_source,
            models_refreshed_at: refresh.models_refreshed_at ?? null,
          }
        : provider
    ),
  }
}
