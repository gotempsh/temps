// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  SERVICE_ALERT_COMPARATOR_OPTIONS,
  type ServiceAlertComparator,
} from './service-alert-comparator'

describe('service alert comparator options', () => {
  test('only offers the symbolic operators the backend accepts', () => {
    // Mirrors validate_comparator in
    // crates/temps-providers/src/handlers/metrics_handlers.rs. Regression
    // test for a bug where the UI sent text forms ("gt", "gte", "lt", "lte")
    // that the handler rejected with 400 Bad Request.
    const backendAccepted = new Set<ServiceAlertComparator>([
      '>',
      '<',
      '>=',
      '<=',
    ])
    const optionValues = SERVICE_ALERT_COMPARATOR_OPTIONS.map((o) => o.value)

    expect(new Set(optionValues)).toEqual(backendAccepted)
  })

  test('has no duplicate values', () => {
    const values = SERVICE_ALERT_COMPARATOR_OPTIONS.map((o) => o.value)
    expect(new Set(values).size).toBe(values.length)
  })
})
