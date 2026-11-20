// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import { DETECTOR_ADVICE, performanceEvidence } from './performance-evidence'

describe('performanceEvidence', () => {
  it('reads what the detector stored on the event', () => {
    const evidence = performanceEvidence({
      trace: {
        extra: {
          performance: {
            detector: 'n_plus_one_query',
            trace_id: '4bf92f3577b34da6a3ce929d0e0e4736',
            span_ids: ['a1', 'a2', 7],
            occurrences: 12,
            duration_ms: 84.5,
            pattern: 'SELECT * FROM items WHERE cart_id = ?',
            operation: 'GET /checkout',
            service: 'shop-api',
          },
        },
      },
    })
    expect(evidence).toEqual({
      detector: 'n_plus_one_query',
      traceId: '4bf92f3577b34da6a3ce929d0e0e4736',
      spanIds: ['a1', 'a2'],
      occurrences: 12,
      durationMs: 84.5,
      pattern: 'SELECT * FROM items WHERE cart_id = ?',
      operation: 'GET /checkout',
      service: 'shop-api',
    })
    expect(DETECTOR_ADVICE[evidence!.detector]).toContain('one query')
  })

  it('is null for an ordinary error event', () => {
    expect(performanceEvidence({ trace: { extra: { tags: {} } } })).toBeNull()
    expect(performanceEvidence(null)).toBeNull()
    expect(performanceEvidence('text')).toBeNull()
  })
})
