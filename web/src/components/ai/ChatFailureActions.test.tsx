// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { ChatFailureActions } from './ChatFailureActions'

function render(code: string, provider = 'codex', busy = false) {
  return renderToStaticMarkup(<MemoryRouter><ChatFailureActions code={code} provider={provider} busy={busy} retryable={code === 'provider_rate_limited'} onRetry={() => {}} onRefresh={async () => {}} /></MemoryRouter>)
}

test('authentication recovery offers local refresh and explicit retry', () => {
  const html = render('harness_authentication_required')
  expect(html).toContain('Refresh from local login')
  expect(html).toContain('Retry message')
  expect(html).toContain('/agent-sandbox/providers/codex')
  expect(render('harness_authentication_required', 'opencode')).toContain('Refresh from local login')
})

test('unsupported providers never offer local credential refresh', () => {
  for (const provider of ['gateway', 'claude_cli']) {
    expect(render('harness_authentication_required', provider)).not.toContain('Refresh from local login')
  }
})

test('rate and quota failures do not suggest credential rotation', () => {
  expect(render('provider_rate_limited')).not.toContain('Refresh from local login')
  const html = render('provider_quota_exhausted')
  expect(html).not.toContain('Refresh from local login')
  expect(html).toContain('Choose another provider')
  expect(html).toContain('Resolve the provider configuration or quota issue before retrying.')
})

test('an active turn disables recovery mutations', () => {
  expect(render('harness_authentication_required', 'codex', true)).toMatch(/disabled=""/)
})
