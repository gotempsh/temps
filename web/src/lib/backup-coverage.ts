// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * What a backup schedule backs up, shared by the create and edit forms and
 * the schedule detail page.
 *
 * The API stores coverage as two flags plus an explicit database list
 * (`target_all_services`, `include_control_plane`, `service_ids`). The
 * console offers three choices on top of them:
 *
 * - `all`: every database, now and later (recommended, the default);
 * - `specific`: the databases picked in the form, with the control plane
 *   as an optional extra;
 * - `control_plane`: only Temps's own database, no databases at all.
 *
 * A schedule is only invalid when it would back up nothing: no databases
 * and the control plane off.
 */

export type BackupCoverageMode = 'all' | 'specific' | 'control_plane'

export interface BackupCoverageFlags {
  target_all_services: boolean
  include_control_plane: boolean
}

export interface BackupCoverageRequest extends BackupCoverageFlags {
  service_ids: number[]
}

export const NOTHING_TO_BACK_UP_MESSAGE =
  'This schedule would back up nothing. Select at least one database, or turn on "Also back up the Temps control plane."'

/** The API fields a form's coverage choice saves as. */
export function coverageRequest(
  mode: BackupCoverageMode,
  selectedServiceIds: number[],
  includeControlPlane: boolean
): BackupCoverageRequest {
  switch (mode) {
    case 'all':
      return {
        target_all_services: true,
        include_control_plane: includeControlPlane,
        service_ids: [],
      }
    case 'specific':
      return {
        target_all_services: false,
        include_control_plane: includeControlPlane,
        service_ids: selectedServiceIds,
      }
    case 'control_plane':
      return {
        target_all_services: false,
        include_control_plane: true,
        service_ids: [],
      }
  }
}

/** Why the chosen coverage cannot be saved, or `undefined` when it can. */
export function coverageError(
  mode: BackupCoverageMode,
  selectedServiceIds: number[],
  includeControlPlane: boolean
): string | undefined {
  const request = coverageRequest(mode, selectedServiceIds, includeControlPlane)
  const backsUpNothing =
    !request.target_all_services &&
    !request.include_control_plane &&
    request.service_ids.length === 0
  return backsUpNothing ? NOTHING_TO_BACK_UP_MESSAGE : undefined
}

/**
 * The form choice that matches a saved schedule. `attachedServiceCount` is
 * the number of databases attached to it; a schedule in specific mode with
 * none attached and the control plane on is a control-plane-only schedule.
 */
export function coverageModeOf(
  schedule: BackupCoverageFlags,
  attachedServiceCount: number
): BackupCoverageMode {
  if (schedule.target_all_services) return 'all'
  if (schedule.include_control_plane && attachedServiceCount === 0) {
    return 'control_plane'
  }
  return 'specific'
}

function databases(count: number): string {
  return count === 1 ? '1 database' : `${count} databases`
}

/** One line describing what every run of a saved schedule backs up. */
export function coverageSummary(
  schedule: BackupCoverageFlags,
  attachedServiceCount: number
): string {
  const withControlPlane = schedule.include_control_plane
    ? ' and the Temps control plane'
    : ''
  if (schedule.target_all_services) {
    return `All databases${withControlPlane}`
  }
  if (attachedServiceCount === 0) {
    return schedule.include_control_plane
      ? 'Only the Temps control plane'
      : 'Nothing: no databases are attached and the control plane is off'
  }
  return `${databases(attachedServiceCount)}${withControlPlane}`
}
