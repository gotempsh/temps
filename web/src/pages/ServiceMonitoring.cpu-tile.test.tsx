// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MetricTile } from './ServiceMonitoring'

describe('container CPU tiles on the monitoring dashboard', () => {
  test('the CPU tile shows cores against the limit and says how fresh it is', () => {
    const markup = renderToStaticMarkup(
      <MetricTile
        name="container.cpu_percent"
        value={68.1}
        display="0.68 / 6 cores"
        selected={false}
      />
    )
    expect(markup).toContain('CPU (cores, last scrape)')
    expect(markup).toContain('previous health check')
    expect(markup).toContain('0.68 / 6 cores')
    expect(markup).not.toContain('68.1%')
  })

  test('without a display override the stored value still reads in cores', () => {
    const markup = renderToStaticMarkup(
      <MetricTile name="container.cpu_percent" value={68.1} selected={false} />
    )
    expect(markup).toContain('0.68 cores')
  })

  test('the utilization tile is a percentage of the limit', () => {
    const markup = renderToStaticMarkup(
      <MetricTile
        name="container.cpu_utilization_percent"
        value={11.35}
        selected={false}
      />
    )
    expect(markup).toContain('CPU (% of limit or host)')
    expect(markup).toContain('11.3%')
  })
})
