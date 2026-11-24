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

/**
 * The metric settings kept aside, read back from a saved draft. A form that
 * leaves for notification setup while showing the log signal saves them
 * beside its draft, so returning and switching back to the metric restores
 * what the user had. Anything that is not a well-formed stash is `null`.
 */
export function parseStashedSignal<A extends string>(
  raw: unknown,
  aggregations: readonly A[]
): SignalFields<A> | null {
  if (typeof raw !== 'object' || raw === null || Array.isArray(raw)) return null
  const value = raw as Record<string, unknown>
  const aggregation = value.aggregation
  const isAggregation =
    aggregation === 'count' ||
    (typeof aggregation === 'string' &&
      (aggregations as readonly string[]).includes(aggregation))
  const isFilter = (item: unknown) =>
    typeof item === 'object' &&
    item !== null &&
    typeof (item as Record<string, unknown>).key === 'string' &&
    typeof (item as Record<string, unknown>).value === 'string'
  if (
    !isAggregation ||
    (value.detection_kind !== 'static' && value.detection_kind !== 'anomaly') ||
    !Array.isArray(value.group_by) ||
    !value.group_by.every((key) => typeof key === 'string') ||
    !Array.isArray(value.label_filters) ||
    !value.label_filters.every(isFilter) ||
    typeof value.dynamic_alerts !== 'boolean'
  ) {
    return null
  }
  return {
    aggregation: aggregation as A | 'count',
    detection_kind: value.detection_kind,
    group_by: value.group_by as string[],
    label_filters: value.label_filters as SignalFields['label_filters'],
    dynamic_alerts: value.dynamic_alerts,
  }
}
