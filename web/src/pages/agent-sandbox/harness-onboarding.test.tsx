// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { ProviderCatalogDto } from '@/api/client'
import { HarnessSetupRow } from './AgentSandboxProvidersList'
import { ProviderEditor } from './AgentSandboxProviderDetail'
import { WorkspaceHarnessSetup } from '@/components/ai-first/WorkspaceHarnessSetup'
import { SetupWizardShell } from '@/components/project/setup/SetupWizardShell'
import {
  harnessSetupHref,
  harnessSectionHref,
  harnessCheckError,
  harnessSetupStatus,
  harnessConnectionMethods,
  initialConnectionMethodId,
  openAiCompatibleCredential,
  openAiCompatibleModelSelection,
  openAiCompatibleUpstreamModel,
  workspaceReturnTo,
  credentialVerificationMessage,
} from './harness-onboarding'

const provider: ProviderCatalogDto = {
  id: 'claude_cli',
  name: 'Claude Code',
  install_command: 'install-cli',
  auth_command: 'login-cli',
  auth_flavors: [],
  models: [],
  runtime_models: [],
  permission_modes: [],
  default_permission_mode_id: 'default',
  credential_saved: false,
  credential_verification_status: 'not_saved',
  host_authenticated: false,
  model_source: 'bootstrap',
  supports_max_turns: true,
  workspace_ready: false,
}

describe('harness onboarding', () => {
  test('never reports an unverified write as verified', () => {
    expect(
      credentialVerificationMessage({
        credential_verification_status: 'unverified',
      })
    ).toContain('not verified')
    expect(credentialVerificationMessage({})).toContain('not verified')
    expect(
      credentialVerificationMessage({
        credential_verification_status: 'verified',
      })
    ).toBe('Credential verified and saved.')
    expect(
      credentialVerificationMessage({
        credential_verification_status: 'unverified',
        verification_hint: 'Choose another model.',
      })
    ).toBe('Choose another model.')
  })
  test('saved OpenCode credentials have a model verification action without re-entering a secret', () => {
    const html = renderToStaticMarkup(
      <QueryClientProvider client={new QueryClient()}>
        <MemoryRouter>
          <ProviderEditor
            provider={{
              ...provider,
              id: 'opencode',
              name: 'OpenCode',
              credential_saved: true,
            }}
            isActive={false}
            embedded
          />
        </MemoryRouter>
      </QueryClientProvider>
    )
    expect(html).toContain('Model to verify')
    expect(html).toContain('Verify saved login')
    expect(html).toContain('not verified')
  })
  test('section navigation preserves only an allowlisted workspace return path', () => {
    const returnTo = '/ai-first?application=example&thread=example-thread'
    for (const path of [
      '/agent-sandbox',
      '/agent-sandbox/providers',
      '/agent-sandbox/sandbox',
      '/agent-sandbox/preview',
      '/agent-sandbox/secrets',
    ]) {
      const href = harnessSectionHref(
        path,
        `?returnTo=${encodeURIComponent(returnTo)}&credential=ignored`
      )
      expect(
        new URL(href, 'https://temps.invalid').searchParams.get('returnTo')
      ).toBe(returnTo)
      expect(href).not.toContain('credential')
      expect(harnessSectionHref(path, '')).toBe(path)
      expect(
        new URL(
          harnessSectionHref(path, '?returnTo=https://example.com'),
          'https://temps.invalid'
        ).searchParams.get('returnTo')
      ).toBe('/ai-first')
    }
  })
  test('Codex shows one method at a time and prefers a detected host login', () => {
    const codex: ProviderCatalogDto = {
      ...provider,
      id: 'codex_cli',
      name: 'Codex',
      auth_flavors: [
        {
          id: 'subscription',
          label: 'ChatGPT subscription',
          description: 'Run `codex login`, then paste `~/.codex/auth.json`.',
          format: 'config_file',
          env_var: null,
        },
        {
          id: 'api_key',
          label: 'API key',
          description: 'Paste an OpenAI API key.',
          format: 'api_key',
          env_var: null,
        },
      ],
      local_credential: {
        auth_type: 'subscription',
        source: 'host_auth_store',
        label: 'Authenticated host CLI',
      },
    }
    const html = renderToStaticMarkup(
      <QueryClientProvider client={new QueryClient()}>
        <MemoryRouter>
          <ProviderEditor provider={codex} isActive={false} />
        </MemoryRouter>
      </QueryClientProvider>
    )
    expect(html).toContain('aria-label="Connection method"')
    expect(html).toContain('Login on this server')
    expect(html).toContain('ChatGPT subscription')
    expect(html).toContain('API key')
    // Only the selected method's form is mounted.
    expect(html).toContain('Import login')
    expect(html).not.toContain('id="cred-codex_cli"')
    expect(html).not.toContain('Login instructions')
  })

  test('connection methods only offer a host login once one is detected', () => {
    const flavors = [
      {
        id: 'subscription',
        label: 'ChatGPT subscription',
        description: 'auth.json',
        format: 'config_file',
      },
      {
        id: 'api_key',
        label: 'API key',
        description: 'key',
        format: 'api_key',
      },
    ]
    const detected = {
      auth_type: 'subscription',
      source: 'host_auth_store',
      label: 'Authenticated host CLI',
    }
    expect(
      harnessConnectionMethods({
        id: 'codex_cli',
        auth_flavors: flavors,
        local_credential: null,
      }).map((method) => method.id)
    ).toEqual(['subscription', 'api_key'])
    expect(
      harnessConnectionMethods({
        id: 'codex_cli',
        auth_flavors: flavors,
        local_credential: detected,
      }).map((method) => method.id)
    ).toEqual(['local', 'subscription', 'api_key'])
    // Claude Code never accepts a host login, even with stale discovery data.
    expect(
      harnessConnectionMethods({
        id: 'claude_cli',
        auth_flavors: flavors,
        local_credential: detected,
      }).map((method) => method.id)
    ).toEqual(['subscription', 'api_key'])
  })

  test('OpenAI-compatible helpers build the stored document and model selection', () => {
    expect(
      JSON.parse(
        openAiCompatibleCredential(' https://api.example.com/v1 ', ' sk-1 ')
      )
    ).toEqual({ base_url: 'https://api.example.com/v1', api_key: 'sk-1' })
    expect(openAiCompatibleModelSelection(' llama-3 ')).toBe(
      'openai-compatible/llama-3'
    )
    expect(
      openAiCompatibleUpstreamModel('openai-compatible/meta-llama/llama-3:free')
    ).toBe('meta-llama/llama-3:free')
    expect(openAiCompatibleUpstreamModel('anthropic/claude-sonnet-4-6')).toBe(
      ''
    )
    expect(openAiCompatibleUpstreamModel(null)).toBe('')
  })

  test('replacing a saved credential preselects the method it was saved with', () => {
    const methods = harnessConnectionMethods({
      id: 'codex_cli',
      auth_flavors: [
        {
          id: 'subscription',
          label: 'S',
          description: '',
          format: 'config_file',
        },
        { id: 'api_key', label: 'K', description: '', format: 'api_key' },
      ],
      local_credential: null,
    })
    expect(
      initialConnectionMethodId(
        { credential_saved: true, current_auth_type: 'api_key' },
        methods
      )
    ).toBe('api_key')
    expect(
      initialConnectionMethodId(
        { credential_saved: false, current_auth_type: 'api_key' },
        methods
      )
    ).toBe('subscription')
    expect(
      initialConnectionMethodId(
        { credential_saved: true, current_auth_type: 'retired' },
        methods
      )
    ).toBe('subscription')
    expect(initialConnectionMethodId({ credential_saved: false }, [])).toBe('')
  })

  test('shared wizard marks completed steps and supports full-width workspace setup', () => {
    const html = renderToStaticMarkup(
      <SetupWizardShell
        title="New workspace"
        description="Setup"
        fullWidth
        currentStep="model"
        steps={[
          { id: 'harness', label: 'Harness' },
          { id: 'model', label: 'Model' },
        ]}
      >
        <p>Choose model</p>
      </SetupWizardShell>
    )
    expect(html).toContain('Harness, completed')
    expect(html).toContain('aria-current="step"')
    expect(html).not.toContain('max-w-3xl')
  })

  test('connection step does not show model controls or ask for a first prompt', () => {
    const html = renderToStaticMarkup(
      <QueryClientProvider client={new QueryClient()}>
        <MemoryRouter>
          <WorkspaceHarnessSetup
            provider={{
              ...provider,
              auth_flavors: [
                {
                  id: 'oauth',
                  label: 'Subscription (OAuth)',
                  description: 'Use a subscription token.',
                  format: 'oauth_token',
                  env_var: null,
                },
                {
                  id: 'api_key',
                  label: 'API Key',
                  description: 'Use an API key.',
                  format: 'api_key',
                  env_var: null,
                },
              ],
            }}
            mode="connection"
            selection={{
              providerId: provider.id,
              modelId: null,
              thinkingOptionId: null,
              permissionModeId: null,
            }}
            onChange={() => {}}
          />
        </MemoryRouter>
      </QueryClientProvider>
    )
    expect(html).toContain('Connect once. Reuse this account')
    expect(html).toContain('aria-label="Connection method"')
    expect(html).toContain('Subscription (OAuth)')
    expect(html).toContain('API Key')
    expect(html).toContain('Use a subscription token.')
    expect(html).toContain('id="cred-claude_cli"')
    expect(html).not.toContain('Login on this server')
    // Tuning and diagnostics stay on the harness page, out of the wizard.
    expect(html).not.toContain('Advanced')
    expect(html).not.toContain('Check setup')
    expect(html).not.toContain('Default model')
    expect(html).not.toContain('workspace-prompt')
    expect(html).not.toContain('Back to workspace')
  })

  test('a saved connection is a one-line summary in the wizard, not a form', () => {
    const html = renderToStaticMarkup(
      <QueryClientProvider client={new QueryClient()}>
        <MemoryRouter>
          <WorkspaceHarnessSetup
            provider={{
              ...provider,
              auth_flavors: [
                {
                  id: 'subscription',
                  label: 'Claude subscription',
                  description: 'Paste a token.',
                  format: 'oauth_token',
                  env_var: null,
                },
              ],
              credential_saved: true,
              credential_verification_status: 'verified',
              current_auth_type: 'subscription',
              workspace_ready: true,
            }}
            mode="connection"
            selection={{
              providerId: provider.id,
              modelId: null,
              thinkingOptionId: null,
              permissionModeId: null,
            }}
            onChange={() => {}}
          />
        </MemoryRouter>
      </QueryClientProvider>
    )
    expect(html).toContain('Connected')
    expect(html).toContain('Claude subscription')
    expect(html).toContain('Replace')
    expect(html).not.toContain('id="cred-claude_cli"')
    expect(html).not.toContain('Use saved Claude Code connection')
  })

  test('model step does not ask for credentials again', () => {
    const html = renderToStaticMarkup(
      <QueryClientProvider client={new QueryClient()}>
        <WorkspaceHarnessSetup
          provider={{
            ...provider,
            workspace_ready: true,
            credential_saved: true,
          }}
          mode="model"
          selection={{
            providerId: provider.id,
            modelId: null,
            thinkingOptionId: null,
            permissionModeId: null,
          }}
          onChange={() => {}}
        />
      </QueryClientProvider>
    )
    expect(html).toContain('Models unavailable')
    expect(html).toContain('Thinking')
    expect(html).not.toContain('Save credential')
  })

  test('keeps underlying problem, network, and proxy errors visible', () => {
    expect(harnessCheckError({ detail: 'Credential expired' })).toBe(
      'Credential expired'
    )
    expect(harnessCheckError(new TypeError('Failed to fetch'))).toBe(
      'Failed to fetch'
    )
    expect(harnessCheckError('Proxy could not connect')).toBe(
      'Proxy could not connect'
    )
    expect(harnessCheckError(null)).toContain('environment check failed')
  })
  test('does not equate configuration with a verified working connection', () => {
    expect(harnessSetupStatus(provider)).toBe('Not connected')
    expect(
      harnessSetupStatus({ credential_saved: true, workspace_ready: true })
    ).toBe('Credential saved')
    expect(
      harnessSetupStatus({ credential_saved: true, workspace_ready: false })
    ).toBe('Needs attention')
  })

  test('returns to the original workspace and thread after setup', () => {
    const destination =
      '/ai-first?application=app_example&thread=thread_example'
    expect(workspaceReturnTo(destination)).toBe(destination)
    const link = new URL(
      harnessSetupHref('claude_cli', destination),
      'https://temps.invalid'
    )
    expect(link.pathname).toBe('/agent-sandbox/providers/claude_cli')
    expect(link.searchParams.get('returnTo')).toBe(destination)
  })

  test('rejects external, malformed, and unrelated setup return paths', () => {
    for (const value of [
      null,
      '//example.com',
      'https://example.com',
      '/\\example.com',
      '/settings',
      '/workspaces/../../settings',
      '/ai-first\n',
      'javascript:alert(1)',
    ]) {
      expect(workspaceReturnTo(value)).toBe('/ai-first')
    }
    expect(workspaceReturnTo('/workspaces/app_example')).toBe(
      '/workspaces/app_example'
    )
  })

  const renderRow = (row: ProviderCatalogDto) =>
    renderToStaticMarkup(
      <MemoryRouter>
        <table>
          <tbody>
            <HarnessSetupRow provider={row} returnTo="/ai-first" />
          </tbody>
        </table>
      </MemoryRouter>
    )

  test('host authentication does not hide missing workspace credentials', () => {
    const html = renderRow({
      ...provider,
      host_authenticated: true,
      host_version: '1.2.3',
    })
    expect(html).toContain('Not connected')
    expect(html).toContain('aria-label="Connect Claude Code"')
    expect(html).not.toContain('Credential saved')
    expect(html).not.toContain('Manage')
    expect(html).not.toContain('Workspace ready')
  })

  test('a harness row links its name to setup and shows how it is connected', () => {
    const html = renderRow({
      ...provider,
      auth_flavors: [
        {
          id: 'oauth_token',
          label: 'Claude subscription',
          description: 'Paste a token.',
          format: 'oauth_token',
        },
      ],
      credential_saved: true,
      current_auth_type: 'oauth_token',
      credential_verification_status: 'verified',
      workspace_ready: true,
    })
    expect(html).toContain('<tr')
    expect(html).toContain('aria-label="Manage Claude Code"')
    expect(html).toContain(
      `href="${harnessSetupHref('claude_cli', '/ai-first')}"`
    )
    expect(html).toContain('Claude subscription')
    expect(html).toContain('Credential saved')
    // The name is the only link: no separate Connect/Manage button.
    expect(html.match(/<a /g)).toHaveLength(1)
  })

  test('setup stays renderable without auth methods and keeps tuning collapsed', () => {
    const html = renderToStaticMarkup(
      <QueryClientProvider client={new QueryClient()}>
        <MemoryRouter>
          <ProviderEditor provider={provider} isActive={false} />
        </MemoryRouter>
      </QueryClientProvider>
    )
    expect(html).toContain('Sign in')
    expect(html).toContain('has no connection methods on this server')
    // Everything beyond signing in is collapsed by default.
    expect(html).toContain('Advanced')
    expect(html).not.toContain('<details open')
    expect(html).toContain('Default model')
    expect(html).toContain(
      'This does not verify a reply in your persistent workspace'
    )
    expect(html).not.toContain('1. Connect your account')
    expect(html).not.toContain('3. Verify your first workspace reply')
  })

  test('OpenCode setup retains the trusted workspace credential disclosure', () => {
    const html = renderToStaticMarkup(
      <QueryClientProvider client={new QueryClient()}>
        <MemoryRouter>
          <ProviderEditor
            provider={{
              ...provider,
              id: 'opencode',
              name: 'OpenCode',
              auth_flavors: [
                {
                  id: 'config_file',
                  label: 'auth.json',
                  description: 'Paste auth.json.',
                  format: 'config_file',
                  env_var: null,
                },
              ],
            }}
            isActive={false}
          />
        </MemoryRouter>
      </QueryClientProvider>
    )
    expect(html).toContain(
      'Code running as the harness user can access this credential'
    )
    expect(html).toContain('Model to verify')
    expect(html).not.toContain('reusable credential is never injected')
  })
})
