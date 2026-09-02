// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** Comparator values accepted by
 *  `POST /external-services/{id}/metrics/alert-rules`. Must stay in sync with
 *  `validate_comparator` in
 *  `crates/temps-providers/src/handlers/metrics_handlers.rs` and the
 *  `monitoring_alert_rules.comparator` CHECK constraint — both only accept
 *  the symbolic operators below, not text forms like `gt`/`gte`. */
export type ServiceAlertComparator = '>' | '>=' | '<' | '<='

export const SERVICE_ALERT_COMPARATOR_OPTIONS: {
  value: ServiceAlertComparator
  label: string
}[] = [
  { value: '>', label: '> greater than' },
  { value: '>=', label: '≥ greater or equal' },
  { value: '<', label: '< less than' },
  { value: '<=', label: '≤ less or equal' },
]
