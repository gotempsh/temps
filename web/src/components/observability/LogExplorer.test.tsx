// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import type { GlobalLogFacetsResponse } from '@/api/client/types.gen'
import { LogExplorer } from './LogExplorer'

test('standalone database logs do not appear as Project 0 or application logs', () => {
  const previousWindow = Object.getOwnPropertyDescriptor(globalThis, 'window')
  Object.defineProperty(globalThis, 'window', {
    configurable: true,
    value: { location: { href: 'http://localhost/logs' } },
  })
  const facets: GlobalLogFacetsResponse = {
    partial: false,
    facets: {
      project_id: [
        { value: '0', count: 100 },
        { value: '7', count: 20 },
      ],
      external_service_id: [{ value: '4', count: 100 }],
    },
    project_names: {},
    external_service_names: {},
  }
  try {
    const html = renderToStaticMarkup(
      <MemoryRouter>
        <LogExplorer lines={[]} facets={facets} onFilter={() => {}} />
      </MemoryRouter>
    )
    expect(html).not.toContain('Project 0')
    expect(html).toContain('Project 7')
    expect(html).toMatch(/Applications<\/span><span[^>]*>20/)
    expect(html).toMatch(/Databases<\/span><span[^>]*>100/)
  } finally {
    if (previousWindow)
      Object.defineProperty(globalThis, 'window', previousWindow)
    else Reflect.deleteProperty(globalThis, 'window')
  }
})

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
    // Facet counts come from the store now, so the sidebar must not keep
    // telling the user they only describe the lines currently loaded.
    expect(html).not.toContain('Counts from this page only')
    expect(html).toContain('Counts across the whole time range')
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
                  container_id: 'sample',
                  line_id: '1',
                  stream: 'stdout',
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
        expect(
          html.includes(
            mode === 'list'
              ? 'whitespace-pre-wrap break-words'
              : 'whitespace-pre-wrap break-all'
          )
        ).toBe(wrap === '1')
      }
    } finally {
      if (previousWindow)
        Object.defineProperty(globalThis, 'window', previousWindow)
      else Reflect.deleteProperty(globalThis, 'window')
    }
  })
}

test('the log list stacks metadata above a full-width message on narrow screens', () => {
  const previousWindow = Object.getOwnPropertyDescriptor(globalThis, 'window')
  Object.defineProperty(globalThis, 'window', {
    configurable: true,
    value: { location: { href: 'http://localhost/logs' } },
  })
  try {
    const html = renderToStaticMarkup(
      <MemoryRouter initialEntries={['/logs?wrap=1']}>
        <LogExplorer
          lines={[
            {
              container_id: 'sample',
              line_id: '1',
              stream: 'stdout',
              timestamp: '2026-09-18T12:00:00Z',
              level: 'INFO',
              owner: 'sample',
              service: 'web',
              env: 'production',
              message: 'relay connection started',
            },
          ]}
          onFilter={() => {}}
        />
      </MemoryRouter>
    )
    expect(html).toContain('block w-full lg:table lg:table-fixed')
    expect(html).toContain('block lg:table-row-group')
    expect(html).toContain('block px-3 py-2 lg:table-row lg:px-0 lg:py-0')
    expect(html).toContain('block min-w-0 p-0 lg:table-cell lg:px-4 lg:py-0.5')
    expect(html).toContain('whitespace-pre-wrap break-words')
    expect(html).toContain('relay connection started')
  } finally {
    if (previousWindow)
      Object.defineProperty(globalThis, 'window', previousWindow)
    else Reflect.deleteProperty(globalThis, 'window')
  }
})

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
      if (visible) expect(html).toContain('Counts across the whole time range')
    } finally {
      if (previousWindow)
        Object.defineProperty(globalThis, 'window', previousWindow)
      else Reflect.deleteProperty(globalThis, 'window')
    }
  })
}

test('facet projects with no line on the loaded page are named from projectLabels', () => {
  const previousWindow = Object.getOwnPropertyDescriptor(globalThis, 'window')
  Object.defineProperty(globalThis, 'window', {
    configurable: true,
    value: { location: { href: 'http://localhost/logs' } },
  })
  try {
    const html = renderToStaticMarkup(
      <MemoryRouter initialEntries={['/logs?facets=1']}>
        <LogExplorer
          lines={[
            {
              container_id: 'sample',
              line_id: '1',
              stream: 'stdout',
              timestamp: '2026-09-18T12:00:00Z',
              level: 'INFO',
              owner: 'web-app',
              project_id: 1,
              service: 'web',
              env: '1',
              message: 'hello',
            },
          ]}
          facets={{
            facets: {
              project_id: [
                { value: '1', count: 10 },
                { value: '2', count: 5 },
                { value: '42', count: 1 },
              ],
            },
            partial: false,
            project_names: { '1': 'web-app', '2': 'api' },
            external_service_names: {},
          }}
          projectLabels={{ '2': 'worker-app', '42': 'Unknown project #42' }}
          onFilter={() => {}}
        />
      </MemoryRouter>
    )
    expect(html).toContain('web-app')
    expect(html).toContain('worker-app')
    expect(html).toContain('Unknown project #42')
    expect(html).not.toContain('Project 2<')
  } finally {
    if (previousWindow)
      Object.defineProperty(globalThis, 'window', previousWindow)
    else Reflect.deleteProperty(globalThis, 'window')
  }
})
