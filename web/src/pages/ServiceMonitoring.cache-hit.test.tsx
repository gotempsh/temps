// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MetricTile } from './ServiceMonitoring'

function tile(scope: Parameters<typeof MetricTile>[0]['scope']) {
  return renderToStaticMarkup(
    <MetricTile
      name="pg.cache_hit_ratio"
      value={0.991}
      selected={false}
      scope={scope}
    />
  )
}

describe('cache hit ratio tile', () => {
  test('the headline names the window and all databases', () => {
    const markup = tile('all')
    expect(markup).toContain('Cache hit ratio (last 15 min, all databases)')
    expect(markup).toContain('weighted by traffic')
    expect(markup).toContain('99.1%')
  })

  test('a per-database tile names the database it shows', () => {
    const markup = tile({ database: 'app_production' })
    expect(markup).toContain('Cache hit ratio (last 15 min, app_production)')
    expect(markup).toContain('in database app_production')
  })

  test('without a scope the label still says what window it covers', () => {
    expect(tile(undefined)).toContain('Cache hit ratio (last 15 min)')
  })

  test('other metrics keep their plain label', () => {
    const markup = renderToStaticMarkup(
      <MetricTile
        name="pg.connections"
        value={4}
        selected={false}
        scope="all"
      />
    )
    expect(markup).toContain('Connections')
    expect(markup).not.toContain('last 15 min')
  })
})
