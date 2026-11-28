// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * The logs page's workload filter, from its URL: what the platform recorded
 * about one workload (the outbound recording card links here with
 * `?workload_kind=sandbox&workload_id=42`). Read by the record's workload
 * columns, which only the platform fills, never by an attribute a project
 * log could carry.
 */
export interface LogWorkloadFilter {
  kind: string
  id: number
}

/** The filter when both halves are present and well-formed. */
export function workloadFilter(
  kind: string | undefined,
  id: string | undefined
): LogWorkloadFilter | undefined {
  if (!kind || !id || !/^[a-z]+$/.test(kind) || !/^[0-9]+$/.test(id)) {
    return undefined
  }
  const numeric = Number(id)
  return Number.isSafeInteger(numeric) ? { kind, id: numeric } : undefined
}
