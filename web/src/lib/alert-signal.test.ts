// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  LOG_SIGNAL_FIELDS,
  parseStashedSignal,
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

describe('parseStashedSignal', () => {
  const aggregations: readonly string[] = ['avg', 'p95']

  test('reads back a stash saved with a draft', () => {
    const saved = JSON.parse(JSON.stringify(metric))
    expect(parseStashedSignal(saved, aggregations)).toEqual(metric)
  })

  test('a restored stash survives the return to the log signal', () => {
    // Metric -> logs -> notification setup -> back: the form mounts on the
    // log signal with the stash read from the draft, then switches back.
    const restored = parseStashedSignal(
      JSON.parse(JSON.stringify(metric)),
      aggregations
    )
    const onMount = switchSignal<SignalFields>(
      true,
      LOG_SIGNAL_FIELDS,
      restored
    )
    const back = switchSignal(false, LOG_SIGNAL_FIELDS, onMount.stashed)
    expect(back.apply).toEqual(metric)
  })

  test('refuses anything else', () => {
    for (const raw of [
      null,
      'p95',
      [],
      { ...metric, aggregation: 'median' },
      { ...metric, detection_kind: 'learned' },
      { ...metric, group_by: [1] },
      { ...metric, label_filters: [{ key: 'region' }] },
      { ...metric, dynamic_alerts: 'yes' },
    ]) {
      expect(parseStashedSignal(raw, aggregations)).toBeNull()
    }
  })
})
