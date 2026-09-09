// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import type { ProviderCatalogDto, ProviderCatalogResponse } from '@/api/client'
import {
  isSavedProviderModelUnavailable,
  mergeProviderModelRefresh,
} from './provider-model-catalog'

describe('provider model catalog refresh', () => {
  test('flags a saved model missing from a refreshed live catalog', () => {
    expect(
      isSavedProviderModelUnavailable({
        savedModel: 'claude-haiku-4-5',
        availableModels: ['sonnet', 'opus', 'haiku'],
        source: 'live',
      })
    ).toBe(true)
  })

  test('does not treat bootstrap or stale fallback catalogs as authoritative', () => {
    for (const source of ['bootstrap', 'stale_cache']) {
      expect(
        isSavedProviderModelUnavailable({
          savedModel: 'custom-account-model',
          availableModels: ['sonnet'],
          source,
        })
      ).toBe(false)
    }
  })

  test('merges only the refreshed provider into the cached catalog', () => {
    const claude = provider('claude_cli')
    const codex = provider('codex_cli')
    const catalog: ProviderCatalogResponse = {
      default_provider: 'claude_cli',
      providers: [claude, codex],
    }

    const refreshed = mergeProviderModelRefresh(catalog, {
      provider_id: 'claude_cli',
      runtime_models: [
        {
          id: 'sonnet',
          name: 'Sonnet 5',
          thinking_modes: [],
          tool_thinking_modes: null,
          default_thinking_mode_id: null,
        },
      ],
      default_runtime_model_id: 'sonnet',
      model_source: 'live',
      models_refreshed_at: '2026-09-08T09:30:00Z',
    })

    expect(refreshed.providers[0].models).toEqual(['sonnet'])
    expect(refreshed.providers[0].model_source).toBe('live')
    expect(refreshed.providers[0].models_refreshed_at).toBe(
      '2026-09-08T09:30:00Z'
    )
    expect(refreshed.providers[1]).toBe(codex)
  })
})

function provider(id: string): ProviderCatalogDto {
  return {
    id,
    name: id,
    install_command: 'install',
    auth_command: 'auth',
    auth_flavors: [],
    models: ['bootstrap'],
    runtime_models: [],
    default_runtime_model_id: 'bootstrap',
    permission_modes: [],
    default_permission_mode_id: 'default',
    credential_saved: true,
    current_auth_type: null,
    default_model: null,
    max_turns_analysis: null,
    max_turns_fix: null,
    max_turns_feedback: null,
    supports_max_turns: id === 'claude_cli',
    host_authenticated: false,
    host_auth_method: null,
    host_version: null,
    model_source: 'bootstrap',
    models_refreshed_at: null,
    host_auth_hint: null,
    workspace_ready: id === 'claude_cli',
    workspace_readiness_hint: null,
  }
}
