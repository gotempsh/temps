// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * The fields a log alert fixes: it counts lines against a static threshold,
 * with no label filter, breakdown or per-series alerting.
 */
export interface SignalFields<Aggregation extends string = string> {
  aggregation: Aggregation | 'count'
  detection_kind: 'static' | 'anomaly'
  group_by: string[]
  label_filters: { key: string; value: string }[]
  dynamic_alerts: boolean
}

export const LOG_SIGNAL_FIELDS: SignalFields = {
  aggregation: 'count',
  detection_kind: 'static',
  group_by: [],
  label_filters: [],
  dynamic_alerts: false,
}

/**
 * Switching an alert form between its metric and log signals.
 *
 * Going to logs sets the fields a log alert fixes and keeps the metric
 * settings aside; coming back restores them, so a round trip through the
 * log signal never changes the metric rule the user returns to. Returns the
 * values to apply (`null`: nothing to change) and what is kept aside.
 */
export function switchSignal<F extends SignalFields>(
  toLogs: boolean,
  current: F,
  stashed: F | null
): { apply: SignalFields | null; stashed: F | null } {
  if (toLogs) {
    return { apply: LOG_SIGNAL_FIELDS, stashed: stashed ?? current }
  }
  return { apply: stashed, stashed: null }
}
