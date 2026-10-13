// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { ProviderCatalogDto } from '@/api/client'
import { HarnessLoginPanel } from './HarnessLoginPanel'

const provider: ProviderCatalogDto = {
  id: 'claude_cli',
  name: 'Claude Code',
  install_command: 'curl -fsSL https://claude.ai/install.sh | bash',
  auth_command: 'claude setup-token',
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
  login_supported: true,
}

function render(overrides: Partial<ProviderCatalogDto> = {}) {
  return renderToStaticMarkup(
    <QueryClientProvider client={new QueryClient()}>
      <HarnessLoginPanel
        provider={{ ...provider, ...overrides }}
        onSucceeded={() => undefined}
      />
    </QueryClientProvider>
  )
}

describe('HarnessLoginPanel', () => {
  test('always offers the sign-in button for a supporting provider', () => {
    const html = render()
    expect(html).toContain('Sign in with Claude')
    expect(html).not.toContain('role="status"')
  })

  test('an unavailable sign-in says what is missing and keeps the button', () => {
    const html = render({
      login_unavailable_reason:
        '`claude` is not installed on this server. Install it with `npm i -g claude`, then refresh this page.',
    })
    expect(html).toContain('is not installed on this server')
    expect(html).toContain('<code')
    expect(html).toContain('Sign in with Claude')
  })

  test('never renders a code field before a sign-in has started', () => {
    const html = render()
    expect(html).not.toContain('Paste code')
  })
})
