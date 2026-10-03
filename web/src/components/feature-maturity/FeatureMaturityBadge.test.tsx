// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'

import { getFeatureMaturityOptions } from '@/api/client/@tanstack/react-query.gen'
import { FeatureMaturityBadge } from './FeatureMaturityBadge'

function render(featureKey: string) {
  const queryClient = new QueryClient()
  queryClient.setQueryData(getFeatureMaturityOptions().queryKey, [
    {
      key: 'ai-chat',
      maturity: 'experimental',
      reason: 'Chat runtimes are still changing.',
      docs_path: '/docs/maturity#experimental',
    },
    {
      key: 'ai-gateway',
      maturity: 'beta',
      reason: 'Gateway routing may change.',
      docs_path: '/docs/maturity#beta',
    },
    {
      key: 'projects',
      maturity: 'stable',
      reason: 'Stable.',
      docs_path: '/docs/maturity#stable',
    },
  ])
  return renderToStaticMarkup(
    <QueryClientProvider client={queryClient}>
      <FeatureMaturityBadge featureKey={featureKey} />
    </QueryClientProvider>
  )
}

describe('FeatureMaturityBadge', () => {
  test('renders an icon trigger with an accessible name instead of a text pill', () => {
    const html = render('ai-chat')

    expect(html).toContain('role="button"')
    expect(html).toContain('aria-label="Experimental feature. Show details"')
    expect(html).toContain('lucide-flask-conical')
    // The label lives in the popover, not in the inline marker.
    expect(html).not.toContain('>Experimental<')
  })

  test('uses a distinct icon for beta features', () => {
    const html = render('ai-gateway')

    expect(html).toContain('aria-label="Beta feature. Show details"')
    expect(html).toContain('lucide-test-tube-diagonal')
    expect(html).not.toContain('lucide-flask-conical')
  })

  test('renders nothing for stable or unknown features', () => {
    expect(render('projects')).toBe('')
    expect(render('does-not-exist')).toBe('')
  })
})
