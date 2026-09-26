// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { TraceUnavailableState } from './TraceUnavailableState'

test('keeps trace context and a recovery action when spans are unavailable', () => {
  const markup = renderToStaticMarkup(
    <TraceUnavailableState
      traceId="fixture-trace-id"
      onBack={() => undefined}
    />
  )

  expect(markup).toContain('Trace data is unavailable')
  expect(markup).toContain('fixture-trace-id')
  expect(markup.match(/Back to traces/g)).toHaveLength(1)
})
