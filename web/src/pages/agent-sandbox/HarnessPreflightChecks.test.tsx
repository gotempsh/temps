// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import {
  HarnessCheckResults,
  HarnessPreflightChecks,
} from './HarnessPreflightChecks'
import type { HarnessCheckReport } from '@/api/client'
import { HarnessCheckProgress } from './HarnessCheckProgress'

describe('Harness diagnostics', () => {
  test('progress exposes a stable status and hides decorative animation from assistive technology', () => {
    const html = renderToStaticMarkup(
      <HarnessCheckProgress label="Checking setup…" />
    )
    expect(html).toContain('role="status"')
    expect(html).toContain('Checking setup…')
    expect(html.match(/harness-check-pixel/g)?.length).toBe(9)
    expect(html).toContain('aria-hidden="true"')
  })
  test('shows skipped checks separately from success, with actionable failure details', () => {
    const report: HarnessCheckReport = {
      provider_id: 'codex_cli',
      mode: 'preflight',
      overall: 'failed',
      diagnostic_id: 'check-123',
      checked_at: '2026-09-15T10:00:00Z',
      checks: [
        {
          id: 'docker',
          label: 'Docker connection',
          status: 'passed',
          detail: 'Connected',
          duration_ms: 120,
        },
        {
          id: 'image',
          label: 'Required image',
          status: 'failed',
          detail: 'Image unavailable',
          action: 'Ask your administrator to pull the required release image.',
          duration_ms: 2100,
        },
        {
          id: 'credential',
          label: 'Credential',
          status: 'not_tested',
          detail: 'Infrastructure must be available first.',
          duration_ms: 7000,
        },
      ],
    }
    const html = renderToStaticMarkup(<HarnessCheckResults report={report} />)
    expect(html).toContain('Setup check: Failed')
    expect(html).toContain('Not tested')
    expect(html).toContain('Ask your administrator to pull')
    expect(html).toContain('check-123')
    expect(html).toContain('2.1s')
    expect(html).not.toContain('7.0s')
    expect(html).not.toContain('Credential verified')
  })

  test.each([true, false])(
    'smoke test starts disabled without explicit consent (saved=%s)',
    (credentialSaved) => {
      const html = renderToStaticMarkup(
        <QueryClientProvider client={new QueryClient()}>
          <HarnessPreflightChecks
            providerId="claude_cli"
            credentialSaved={credentialSaved}
          />
        </QueryClientProvider>
      )
      expect(html).toMatch(
        /<button[^>]*disabled=""[^>]*>Run smoke test<\/button>/
      )
      expect(html).toContain('provider allowance')
      expect(html).toContain('No model request is sent during the setup check')
      if (!credentialSaved)
        expect(html).toContain('Save a credential to run the smoke test')
    }
  )

  test('renders technical detail as text, never HTML', () => {
    const html = renderToStaticMarkup(
      <HarnessCheckResults
        report={{
          provider_id: 'codex_cli',
          mode: 'smoke',
          overall: 'failed',
          checked_at: '2026-09-15T10:00:00Z',
          diagnostic_id: 'safe',
          checks: [
            {
              id: 'model',
              label: 'Model reply',
              status: 'failed',
              detail: '<script>alert(1)</script>',
              duration_ms: 0,
            },
          ],
        }}
      />
    )
    expect(html).not.toContain('<script>')
    expect(html).toContain('&lt;script&gt;')
  })
})
