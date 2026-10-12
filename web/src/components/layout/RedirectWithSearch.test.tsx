// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import {
  MemoryRouter,
  Route,
  Routes,
  createPath,
  useLocation,
  useParams,
} from 'react-router'
import { buildRedirectTarget } from '@/lib/redirect-target'
import { RedirectWithSearch } from './RedirectWithSearch'

function target(
  to: string,
  location: string,
  params: Record<string, string> = {}
) {
  const [pathAndSearch, hash = ''] = location.split('#')
  const [, search = ''] = pathAndSearch.split('?')
  return createPath(
    buildRedirectTarget({
      to,
      search: search ? `?${search}` : '',
      hash: hash ? `#${hash}` : '',
      params,
    })
  )
}

describe('buildRedirectTarget', () => {
  test('carries the query string over to the new location', () => {
    expect(target('/monitoring/alarms', '/alarms?search=x&tab=y')).toBe(
      '/monitoring/alarms?search=x&tab=y'
    )
  })

  test('carries the hash over', () => {
    expect(target('/domains', '/settings/domains?q=a#row-3')).toBe(
      '/domains?q=a#row-3'
    )
  })

  test('adds nothing when the old link had no query', () => {
    expect(target('/proxy', '/monitoring/resources')).toBe('/proxy')
  })

  test('keeps relative targets relative', () => {
    expect(target('../settings/delivery', '/projects/a/hosting?x=1')).toBe(
      '../settings/delivery?x=1'
    )
  })

  test('destination query wins over a clashing incoming key but both survive', () => {
    expect(
      target('/projects/new?source=drop', '/drop?source=git&ref=main')
    ).toBe('/projects/new?source=drop&ref=main')
  })

  test('fills :param and * segments from the matched route', () => {
    expect(
      target('/new/:id/runs/*', '/old/42/runs/9/logs?tab=y', {
        id: '42',
        '*': '9/logs',
      })
    ).toBe('/new/42/runs/9/logs?tab=y')
  })
})

describe('RedirectWithSearch inside a router', () => {
  test('param routes resolve against the real matched params and search', () => {
    let resolved = ''
    function Probe() {
      const { search, hash } = useLocation()
      const params = useParams()
      resolved = createPath(
        buildRedirectTarget({ to: '/runs/:runId', search, hash, params })
      )
      return null
    }
    renderToStaticMarkup(
      <MemoryRouter initialEntries={['/legacy/run-7?filter=failed&page=2#top']}>
        <Routes>
          <Route path="/legacy/:runId" element={<Probe />} />
        </Routes>
      </MemoryRouter>
    )
    expect(resolved).toBe('/runs/run-7?filter=failed&page=2#top')
  })

  test('the component renders on a param route without throwing', () => {
    const markup = renderToStaticMarkup(
      <MemoryRouter initialEntries={['/legacy/run-7?filter=failed']}>
        <Routes>
          <Route
            path="/legacy/:runId"
            element={<RedirectWithSearch to="/runs/:runId" />}
          />
        </Routes>
      </MemoryRouter>
    )
    expect(markup).toBe('')
  })
})
