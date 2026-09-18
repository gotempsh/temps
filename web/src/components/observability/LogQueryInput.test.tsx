// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { LogQueryInput } from './LogQueryInput'

test('an ID-backed environment filter displays its slug in the chip and clear action', () => {
  const params = new URLSearchParams('env=2')
  const html = renderToStaticMarkup(
    <QueryClientProvider client={new QueryClient()}>
      <LogQueryInput
        params={params}
        text=""
        lines={[]}
        environmentLabels={{ '2': 'production' }}
        onChange={() => {}}
      />
    </QueryClientProvider>
  )
  expect(html).toContain('env:production')
  expect(html).toContain('Clear environment: production')
  expect(html).not.toContain('env:2')
  expect(params.get('env')).toBe('2')
})
