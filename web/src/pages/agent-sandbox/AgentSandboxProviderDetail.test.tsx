// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'

import type { ProviderCatalogDto } from '@/api/client'
import { ProviderEditor } from './AgentSandboxProviderDetail'

describe('ProviderEditor local credential onboarding', () => {
  test('discloses native OpenCode credential access instead of promising relay isolation', () => {
    const html = renderToStaticMarkup(
      <QueryClientProvider client={new QueryClient()}>
        <MemoryRouter>
          <ProviderEditor
            provider={{ ...provider, id: 'opencode', name: 'OpenCode' }}
            isActive={false}
          />
        </MemoryRouter>
      </QueryClientProvider>
    )
    expect(html).toContain('private runtime credential file')
    expect(html).toContain('only use it in workspaces you trust')
    expect(html).toContain(
      'after replacing the sandbox, you may need to import your local login again'
    )
    expect(html).not.toContain('credential is never injected into the sandbox')
  })
  test.each([false, true])(
    'never offers Claude local import even with stale discovery metadata (saved=%s)',
    (saved) => {
      const html = renderToStaticMarkup(
        <QueryClientProvider client={new QueryClient()}>
          <MemoryRouter>
            <ProviderEditor
              provider={{
                ...provider,
                id: 'claude_cli',
                name: 'Claude Code',
                credential_saved: saved,
              }}
              isActive={false}
            />
          </MemoryRouter>
        </QueryClientProvider>
      )
      expect(html).not.toContain('Use local login')
      expect(html).not.toContain('Replace with local login')
      expect(html).not.toContain('Temps found an authenticated')
      expect(html).toContain('claude setup-token')
      expect(html).toContain('Anthropic API key')
    }
  )
  test('offers one-click import without rendering credential material', () => {
    const html = renderToStaticMarkup(
      <QueryClientProvider client={new QueryClient()}>
        <MemoryRouter initialEntries={['/?connectionMethod=local']}>
          <ProviderEditor provider={provider} isActive={false} />
        </MemoryRouter>
      </QueryClientProvider>
    )

    expect(html).toContain('Use local Codex login')
    expect(html).toContain('A login was detected on the Temps host')
    expect(html).toContain('without exposing the credential to this browser')
    expect(html).not.toContain('oauth-secret')
    expect(html).not.toContain('.credentials.json')
  })

  test('labels import as a replacement when a credential is already saved', () => {
    const html = renderToStaticMarkup(
      <QueryClientProvider client={new QueryClient()}>
        <MemoryRouter initialEntries={['/?connectionMethod=local']}>
          <ProviderEditor
            provider={{ ...provider, credential_saved: true }}
            isActive={false}
          />
        </MemoryRouter>
      </QueryClientProvider>
    )

    expect(html).toContain('Verify &amp; replace login')
  })
})

const provider: ProviderCatalogDto = {
  id: 'codex_cli',
  name: 'Codex',
  install_command: 'install claude',
  auth_command: 'claude setup-token',
  auth_flavors: [
    {
      id: 'api_key',
      label: 'API key',
      description: 'API key',
      format: 'api_key',
    },
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
  credential_verification_status: 'not_saved',
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
