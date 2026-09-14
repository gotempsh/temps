// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { LogExplorer } from './LogExplorer'

test('shows loaded logs without a misleading current-page volume chart', () => {
  const previousWindow = Object.getOwnPropertyDescriptor(globalThis, 'window')
  Object.defineProperty(globalThis, 'window', {
    configurable: true,
    value: { location: { href: 'http://localhost/logs' } },
  })
  try {
    const html = renderToStaticMarkup(
      <MemoryRouter>
        <LogExplorer lines={[]} onFilter={() => {}} />
      </MemoryRouter>
    )
    expect(html).toContain('loaded')
    expect(html).not.toContain('Volume by level')
    expect(html).not.toContain('Show volume table')
  } finally {
    if (previousWindow)
      Object.defineProperty(globalThis, 'window', previousWindow)
    else Reflect.deleteProperty(globalThis, 'window')
  }
})
