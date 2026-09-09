// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'

import type { ProviderCatalogDto } from '@/api/client'
import { ProviderEditor } from './AgentSandboxProviderDetail'

describe('ProviderEditor local credential onboarding', () => {
  test('offers one-click import without rendering credential material', () => {
    const html = renderToStaticMarkup(
      <QueryClientProvider client={new QueryClient()}>
        <MemoryRouter>
          <ProviderEditor provider={provider} isActive={false} />
        </MemoryRouter>
      </QueryClientProvider>
    )

    expect(html).toContain('Use local login')
    expect(html).toContain('authenticated Claude Code credential')
    expect(html).toContain('without exposing it to this browser')
    expect(html).not.toContain('oauth-secret')
    expect(html).not.toContain('.credentials.json')
  })

  test('labels import as a replacement when a credential is already saved', () => {
    const html = renderToStaticMarkup(
      <QueryClientProvider client={new QueryClient()}>
        <MemoryRouter>
          <ProviderEditor
            provider={{ ...provider, credential_saved: true }}
            isActive={false}
          />
        </MemoryRouter>
      </QueryClientProvider>
    )

    expect(html).toContain('Replace with local login')
  })
})

const provider: ProviderCatalogDto = {
  id: 'claude_cli',
  name: 'Claude Code',
  install_command: 'install claude',
  auth_command: 'claude setup-token',
  auth_flavors: [
    {
      id: 'subscription',
      label: 'Subscription (OAuth)',
      description: 'Claude subscription credential',
      format: 'oauth_token',
    },
  ],
  models: ['sonnet'],
  runtime_models: [],
  default_runtime_model_id: 'sonnet',
  permission_modes: [],
  default_permission_mode_id: 'default',
  credential_saved: false,
  current_auth_type: null,
  default_model: null,
  max_turns_analysis: null,
  max_turns_fix: null,
  max_turns_feedback: null,
  supports_max_turns: true,
  host_authenticated: true,
  host_auth_method: 'host_auth_store',
  host_version: '2.1.0',
  model_source: 'bootstrap',
  models_refreshed_at: null,
  host_auth_hint: null,
  workspace_ready: false,
  workspace_readiness_hint: 'Save a Claude Code credential.',
  local_credential: {
    auth_type: 'subscription',
    source: 'host_auth_store',
    label: 'Authenticated host CLI',
  },
}
