// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** Missing history metadata is unknown; only explicit NULL identifies a worker. */
export function containerWorkload(
  container: { container_port?: number | null } | null | undefined
): 'worker' | 'service' | 'unknown' {
  if (container?.container_port === null) return 'worker'
  return container?.container_port === undefined ? 'unknown' : 'service'
}
