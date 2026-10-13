// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'

import type { ProviderCatalogDto } from '@/api/client'
import { ProviderEditor } from './AgentSandboxProviderDetail'

function renderEditor(
  overrides: Partial<ProviderCatalogDto>,
  initialEntries = ['/']
) {
  return renderToStaticMarkup(
    <QueryClientProvider client={new QueryClient()}>
      <MemoryRouter initialEntries={initialEntries}>
        <ProviderEditor
          provider={{ ...provider, ...overrides }}
          isActive={false}
        />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

describe('ProviderEditor local credential onboarding', () => {
  test('discloses native OpenCode credential access instead of promising relay isolation', () => {
    const html = renderEditor({
      id: 'opencode',
      name: 'OpenCode',
      auth_flavors: [
        {
          id: 'config_file',
          label: 'auth.json',
          description: 'Paste auth.json.',
          format: 'config_file',
        },
      ],
    })
    expect(html).toContain('private runtime credential file')
    expect(html).toContain('only use it in workspaces you trust')
    expect(html).toContain(
      'after replacing the sandbox, you may need to import your local login again'
    )
    expect(html).not.toContain('never receive the credential')
  })

  test('promises relay isolation for relay-backed harnesses', () => {
    const html = renderEditor({ local_credential: null })
    expect(html).toContain('never receive the credential')
    expect(html).not.toContain('private runtime credential file')
  })

  test('never offers Claude local import even with stale discovery metadata', () => {
    const html = renderEditor({
      id: 'claude_cli',
      name: 'Claude Code',
      auth_flavors: [
        {
          id: 'subscription',
          label: 'Claude subscription',
          description: 'Run `claude setup-token`, then paste the token.',
          format: 'oauth_token',
        },
        {
          id: 'api_key',
          label: 'API key',
          description: 'Paste an Anthropic API key.',
          format: 'api_key',
        },
      ],
    })
    expect(html).not.toContain('Login on this server')
    expect(html).not.toContain('Import login')
    expect(html).not.toContain('Temps found a')
    expect(html).toContain('<code')
    expect(html).toContain('claude setup-token')
    expect(html).toContain('API key')
    expect(html).toContain('id="cred-claude_cli"')
  })

  test('offers one-click import without rendering credential material', () => {
    const html = renderEditor({})

    expect(html).toContain('Temps found a Codex login on this server')
    expect(html).toContain('without exposing it to this browser')
    expect(html).toContain('Import login')
    expect(html).not.toContain('oauth-secret')
    expect(html).not.toContain('.credentials.json')
  })

  test('falls back to pasting a credential when no host login is detected', () => {
    const html = renderEditor({ local_credential: null })

    expect(html).not.toContain('Login on this server')
    expect(html).toContain('id="cred-codex_cli"')
    expect(html).toContain('type="password"')
    expect(html).toContain('>Connect<')
  })

  test('collapses a saved connection to a summary instead of an empty form', () => {
    const html = renderEditor({
      credential_saved: true,
      credential_verification_status: 'verified',
      current_auth_type: 'api_key',
      workspace_ready: true,
    })

    expect(html).toContain('Connected')
    expect(html).toContain('API key')
    expect(html).toContain('Replace')
    expect(html).not.toContain('id="cred-codex_cli"')
    expect(html).not.toContain('Import login')
  })

  test('never presents an unverified saved credential as connected', () => {
    const html = renderEditor({
      credential_saved: true,
      credential_verification_status: 'unverified',
      current_auth_type: 'api_key',
      workspace_ready: false,
    })

    expect(html).toContain('Saved, not verified')
    expect(html).toContain('not verified')
    expect(html).not.toContain('>Connected<')
  })
})

const compatibleFlavor = {
  id: 'openai_compatible',
  label: 'OpenAI-compatible API',
  description: 'Any public HTTPS endpoint that speaks Chat Completions.',
  format: 'openai_compatible',
}

describe('OpenCode OpenAI-compatible endpoint', () => {
  test('asks for base URL, key and model and promises relay isolation', () => {
    const html = renderEditor({
      id: 'opencode',
      name: 'OpenCode',
      local_credential: null,
      auth_flavors: [compatibleFlavor],
    })

    expect(html).toContain('Base URL')
    expect(html).toContain('placeholder="https://openrouter.ai/api/v1"')
    expect(html).toContain('id="cred-opencode"')
    expect(html).toContain('type="password"')
    expect(html).toContain('id="endpoint-model-opencode"')
    // The key never enters the sandbox, unlike native auth.json.
    expect(html).toContain('never receive the credential')
    expect(html).not.toContain('private runtime credential file')
    expect(html).not.toContain('No CLI yet?')
    expect(html).not.toContain('Model to verify')
  })

  test('offers the endpoint next to auth.json without mounting both forms', () => {
    const html = renderEditor({
      id: 'opencode',
      name: 'OpenCode',
      local_credential: null,
      auth_flavors: [
        {
          id: 'config_file',
          label: 'auth.json',
          description: 'Paste auth.json.',
          format: 'config_file',
        },
        compatibleFlavor,
      ],
    })

    expect(html).toContain('OpenAI-compatible API')
    expect(html).toContain('auth.json')
    // auth.json is first, so its textarea is the mounted form.
    expect(html).toContain('<textarea')
    expect(html).not.toContain('id="endpoint-url-opencode"')
    expect(html).toContain('private runtime credential file')
  })

  test('re-verifies a saved endpoint by its bare model id', () => {
    const html = renderEditor({
      id: 'opencode',
      name: 'OpenCode',
      local_credential: null,
      auth_flavors: [compatibleFlavor],
      credential_saved: true,
      credential_verification_status: 'unverified',
      current_auth_type: 'openai_compatible',
      default_model: 'openai-compatible/llama-3.3-70b',
      workspace_ready: false,
    })

    expect(html).toContain('Saved, not verified')
    expect(html).toContain('OpenAI-compatible API')
    expect(html).toContain('value="llama-3.3-70b"')
    expect(html).toContain('Verify saved login')
    expect(html).not.toContain('<option value=""')
    expect(html).not.toContain('Model to verify')
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
