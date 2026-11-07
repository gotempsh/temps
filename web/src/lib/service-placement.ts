// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Where a new standalone service runs, as the create form offers it.
 *
 * A control plane started with `--profile control-plane` runs no containers
 * (`local_workloads: false` in the node capability), so offering it as a
 * target only produces a rejected create. There the form offers workers
 * only and starts on the first one; elsewhere it behaves as before (the
 * control plane, unless the user picks a worker).
 */

/** Select value of the control plane. */
export const CONTROL_PLANE_PLACEMENT = 'control-plane'

export interface StandalonePlacement {
  /** Whether the control plane is listed as a target. */
  offerControlPlane: boolean
  /** The selected value: a worker id, the control plane, or none. */
  value: string | null
  /** No target can run the service (no local workloads, no worker). */
  noTarget: boolean
}

/**
 * The placement to show and submit.
 *
 * `chosen` is what the user picked, if anything. `localWorkloads` is the
 * capability's flag; `undefined` (still loading, or a server without the
 * endpoint) keeps the previous behaviour.
 */
export function standalonePlacement(
  chosen: string | null,
  localWorkloads: boolean | undefined,
  workerIds: readonly number[]
): StandalonePlacement {
  const offerControlPlane = localWorkloads !== false
  const workers = workerIds.map(String)
  const valid =
    chosen !== null &&
    (chosen === CONTROL_PLANE_PLACEMENT
      ? offerControlPlane
      : workers.includes(chosen))
  if (valid) {
    return { offerControlPlane, value: chosen, noTarget: false }
  }
  if (offerControlPlane) {
    return {
      offerControlPlane,
      value: CONTROL_PLANE_PLACEMENT,
      noTarget: false,
    }
  }
  const first = workers[0] ?? null
  return { offerControlPlane, value: first, noTarget: first === null }
}

/** The `node_id` a placement value sends: `null` for the control plane. */
export function placementNodeId(value: string | null): number | null {
  if (value === null || value === CONTROL_PLANE_PLACEMENT) return null
  const id = Number(value)
  return Number.isInteger(id) ? id : null
}
