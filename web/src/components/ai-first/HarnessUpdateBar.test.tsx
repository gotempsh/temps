// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { HarnessUpdateBar } from './HarnessUpdateBar'

function render(status: Record<string, unknown> | undefined) {
  const client = new QueryClient()
  if (status) client.setQueryData(['harness-update', 'claude_cli'], status)
  return renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <HarnessUpdateBar
        providerId="claude_cli"
        providerName="Claude Code"
        enabled={false}
      />
    </QueryClientProvider>
  )
}

const base = {
  provider_id: 'claude_cli',
  installed_version: '2.1.263',
  latest_version: '2.1.296',
  update_available: true,
  updating: false,
}

describe('HarnessUpdateBar', () => {
  test('offers the update with the version change', () => {
    const html = render(base)
    expect(html).toContain('Claude Code update available')
    expect(html).toContain('2.1.263 → 2.1.296')
    expect(html).toContain('Update now')
  })

  test('says it is updating and disables the action', () => {
    const html = render({ ...base, updating: true })
    expect(html).toContain('Updating Claude Code…')
    expect(html).toContain('disabled')
  })

  test('explains a check that could not tell and offers to retry', () => {
    const html = render({
      provider_id: 'claude_cli',
      installed_version: null,
      latest_version: null,
      update_available: false,
      updating: false,
      detail: 'The workspace is not running.',
    })
    expect(html).toContain('check for a Claude Code update')
    expect(html).toContain('The workspace is not running.')
    expect(html).toContain('Check again')
  })

  test('renders nothing when the harness is current or unknown', () => {
    expect(render({ ...base, update_available: false })).toBe('')
    expect(render(undefined)).toBe('')
  })
})
