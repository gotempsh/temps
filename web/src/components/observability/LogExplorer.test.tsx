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

for (const mode of ['list', 'patterns', 'service']) {
  test(`wrap preference applies to ${mode} when restored from the URL`, () => {
    const previousWindow = Object.getOwnPropertyDescriptor(globalThis, 'window')
    Object.defineProperty(globalThis, 'window', {
      configurable: true,
      value: { location: { href: 'http://localhost/logs' } },
    })
    try {
      for (const wrap of ['0', '1']) {
        const html = renderToStaticMarkup(
          <MemoryRouter initialEntries={[`/logs?lv=${mode}&wrap=${wrap}`]}>
            <LogExplorer
              lines={[
                {
                  chunk_id: 'sample',
                  line_offset: 0,
                  timestamp: '2026-09-18T12:00:00Z',
                  level: 'INFO',
                  owner: 'sample',
                  service: 'web',
                  env: 'production',
                  message: 'Long message '.repeat(30),
                },
              ]}
              onFilter={() => {}}
            />
          </MemoryRouter>
        )
        expect(html.includes('whitespace-pre-wrap break-all')).toBe(
          wrap === '1'
        )
      }
    } finally {
      if (previousWindow)
        Object.defineProperty(globalThis, 'window', previousWindow)
      else Reflect.deleteProperty(globalThis, 'window')
    }
  })
}

for (const [query, visible] of [
  ['', true],
  ['?facets=0', false],
  ['?facets=1', true],
] as const) {
  test(`desktop facet counts respect the URL preference: ${query || 'default'}`, () => {
    const previousWindow = Object.getOwnPropertyDescriptor(globalThis, 'window')
    Object.defineProperty(globalThis, 'window', {
      configurable: true,
      value: { location: { href: 'http://localhost/logs' } },
    })
    try {
      const html = renderToStaticMarkup(
        <MemoryRouter initialEntries={[`/logs${query}`]}>
          <LogExplorer lines={[]} onFilter={() => {}} />
        </MemoryRouter>
      )
      expect(html.includes('aria-label="Log facets"')).toBe(visible)
      expect(html.includes('aria-expanded="true"')).toBe(visible)
      if (visible) expect(html).toContain('Counts from this page only.')
    } finally {
      if (previousWindow)
        Object.defineProperty(globalThis, 'window', previousWindow)
      else Reflect.deleteProperty(globalThis, 'window')
    }
  })
}
