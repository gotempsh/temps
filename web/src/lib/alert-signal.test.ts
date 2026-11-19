// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  LOG_SIGNAL_FIELDS,
  switchSignal,
  type SignalFields,
} from './alert-signal'

const metric: SignalFields = {
  aggregation: 'p95',
  detection_kind: 'anomaly',
  group_by: ['service'],
  label_filters: [{ key: 'region', value: 'eu' }],
  dynamic_alerts: true,
}

describe('switchSignal', () => {
  test('a round trip through logs restores the metric settings', () => {
    const toLogs = switchSignal(true, metric, null)
    expect(toLogs.apply).toEqual(LOG_SIGNAL_FIELDS)
    expect(toLogs.stashed).toBe(metric)

    const back = switchSignal(false, LOG_SIGNAL_FIELDS, toLogs.stashed)
    expect(back.apply).toBe(metric)
    expect(back.stashed).toBeNull()
  })

  test('staying on logs keeps the first stash', () => {
    const first = switchSignal(true, metric, null)
    const again = switchSignal(true, LOG_SIGNAL_FIELDS, first.stashed)
    expect(again.stashed).toBe(metric)
  })

  test('a metric form that never visited logs is left alone', () => {
    expect(switchSignal(false, metric, null)).toEqual({
      apply: null,
      stashed: null,
    })
  })
})
