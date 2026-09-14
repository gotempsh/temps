// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { ProviderCatalogDto } from '@/api/client'
import { HarnessSetupCard } from './AgentSandboxProvidersList'
import { ProviderEditor } from './AgentSandboxProviderDetail'
import { WorkspaceHarnessSetup } from '@/components/ai-first/WorkspaceHarnessSetup'
import { SetupWizardShell } from '@/components/project/setup/SetupWizardShell'
import {
  harnessSetupHref,
  harnessSectionHref,
  harnessCheckError,
  harnessSetupStatus,
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
  test('Codex offers three connection cards without exposing every form', () => {
    const html = renderToStaticMarkup(
      <QueryClientProvider client={new QueryClient()}>
        <MemoryRouter>
          <ProviderEditor
            provider={{
              ...provider,
              id: 'codex_cli',
              name: 'Codex',
              auth_flavors: [
                {
                  id: 'subscription',
                  label: 'Subscription',
                  description: 'auth.json',
                  format: 'config_file',
                  env_var: null,
                },
                {
                  id: 'api_key',
                  label: 'API key',
                  description: 'OpenAI key',
                  format: 'api_key',
                  env_var: null,
                },
              ],
            }}
            isActive={false}
          />
        </MemoryRouter>
      </QueryClientProvider>
    )
    expect(html).toContain('Use local login')
    expect(html).toContain('Subscription')
    expect(html).toContain('OpenAI API key')
    expect(html).not.toContain('id="cred-codex_cli"')
    expect(html).not.toContain('Login instructions')
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
    expect(html).toContain('Connection methods')
    expect(html).toContain('Subscription')
    expect(html).toContain('Anthropic API key')
    expect(html).not.toContain('role="tablist"')
    expect(html).not.toContain('Use local login')
    expect(html).not.toContain('id="cred-claude_cli"')
    expect(html).not.toContain('aria-pressed=')
    expect(html).toContain('border-0 shadow-none rounded-none')
    expect(html).not.toContain('How do I get a credential?')
    expect(html).not.toContain('<details open=')
    expect(html).not.toContain('Check environment')
    expect(html).not.toContain('2. Choose a model')
    expect(html).not.toContain('workspace-prompt')
    expect(html).not.toContain('Back to workspace')
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

  test('host authentication does not hide missing workspace credentials', () => {
    const html = renderToStaticMarkup(
      <MemoryRouter>
        <HarnessSetupCard
          provider={{
            ...provider,
            host_authenticated: true,
            host_version: '1.2.3',
          }}
          returnTo="/ai-first"
        />
      </MemoryRouter>
    )
    expect(html).toContain('Not connected')
    expect(html).toContain('Authenticated')
    expect(html).toContain('Not saved')
    expect(html).toContain('Connect harness')
    expect(html).not.toContain('Workspace ready')
  })

  test('setup stays renderable without auth methods and explains verification scope', () => {
    const html = renderToStaticMarkup(
      <QueryClientProvider client={new QueryClient()}>
        <MemoryRouter>
          <ProviderEditor provider={provider} isActive={false} />
        </MemoryRouter>
      </QueryClientProvider>
    )
    expect(html).toContain('1. Connect your account')
    expect(html).toContain('2. Choose a model')
    expect(html).toContain('3. Verify your first workspace reply')
    expect(html).toContain('claude setup-token')
    expect(html).toContain('not in the workspace terminal')
    expect(html).toContain(
      'This does not verify a reply in your persistent workspace'
    )
    expect(html).toContain('Advanced: instance default and autofix limits')
  })

  test('OpenCode setup retains the trusted workspace credential disclosure', () => {
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
    expect(html).toContain(
      'Code running as the harness user can access this credential'
    )
    expect(html).not.toContain('reusable credential is never injected')
  })
})
