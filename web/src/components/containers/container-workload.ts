// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** Missing history metadata is unknown; only explicit NULL identifies a worker. */
export function containerWorkload(
  container: { container_port?: number | null } | null | undefined
): 'worker' | 'service' | 'unknown' {
  if (container?.container_port === null) return 'worker'
  return container?.container_port === undefined ? 'unknown' : 'service'
}

/** Aggregate recorded deploy health only when every current container is portless. */
export function portlessWorkerHealth(
  containers:
    | ReadonlyArray<{
        container_port?: number | null
        health_state?: string
      }>
    | undefined
): 'healthy' | 'unhealthy' | 'unknown' | null {
  if (
    !containers?.length ||
    containers.some((container) => containerWorkload(container) !== 'worker')
  )
    return null
  if (containers.some((container) => container.health_state === 'unhealthy'))
    return 'unhealthy'
  return containers.every((container) => container.health_state === 'healthy')
    ? 'healthy'
    : 'unknown'
}
