// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { NoNodeSamplesState } from './NoNodeSamplesState'

function render() {
  return renderToStaticMarkup(
    <MemoryRouter>
      <NoNodeSamplesState
        range="6h"
        scrapeInterval={30}
        settingsPath="/settings/metrics-monitoring"
      />
    </MemoryRouter>
  )
}

test('says what is missing, for which window, and when to expect it', () => {
  const html = render()
  expect(html).toContain('No server samples in this window')
  expect(html).toContain('in the last 6h')
  expect(html).toContain('every 30 s')
})

test('deep-links to the collection settings instead of leaving a blank panel', () => {
  const html = render()
  expect(html).toContain('href="/settings/metrics-monitoring"')
  expect(html).toContain('Metrics monitoring settings')
})

test('uses the compact empty state on a single bordered surface', () => {
  const html = render()
  expect(html).toContain('min-h-60')
  expect(html).not.toContain('min-h-[400px]')
})

test('worker wording names the agent heartbeat and leaves out the network the agent does not report', () => {
  const html = renderToStaticMarkup(
    <MemoryRouter>
      <NoNodeSamplesState
        range="1h"
        scrapeInterval={30}
        settingsPath="/settings/metrics-monitoring"
        nodeKind="worker"
      />
    </MemoryRouter>
  )
  expect(html).toContain('No worker samples in this window')
  expect(html).toContain('every heartbeat')
  expect(html).toContain('temps agent')
  expect(html).not.toContain('network')
})
