// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { ProjectResponse } from '@/api/client'
import { SecurityScansHeader } from './SecurityOverview'

function render(enabled: boolean) {
  const project = {
    id: 7,
    slug: 'shop',
    vulnerability_scanning_enabled: enabled,
  } as ProjectResponse
  return renderToStaticMarkup(
    <QueryClientProvider client={new QueryClient()}>
      <SecurityScansHeader project={project} />
    </QueryClientProvider>
  )
}

describe('SecurityScansHeader', () => {
  test('offers an on/off switch on the scans page itself', () => {
    const html = render(false)
    expect(html).toContain('role="switch"')
    expect(html).toContain('id="vulnerability-scanning"')
    expect(html).toContain('for="vulnerability-scanning"')
    expect(html).toContain('aria-checked="false"')
    expect(html).not.toContain('/settings/security')
  })

  test('reflects the project setting when scanning is on', () => {
    expect(render(true)).toContain('aria-checked="true"')
  })
})
