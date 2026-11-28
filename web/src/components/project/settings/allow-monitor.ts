// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * "Allow monitor": add the addresses Temps' uptime monitor connects from to
 * the IP access list that refuses it, keeping every other entry.
 *
 * The list is replaced as a whole, so the save is made against a fresh read
 * and carries the revision it read: a save someone else made in between is
 * refused by the server (never overwritten), and the action re-reads and
 * decides again.
 */
import {
  getEnvironmentIpAccess,
  getProjectIpAccess,
  putEnvironmentIpAccess,
  putProjectIpAccess,
  type ProjectIpAccessResponse,
  type SetProjectIpAccessRequest,
  type UptimeMonitorIpBlock,
} from '@/api/client'
import {
  isRevisionConflict,
  planMonitorFix,
  type MonitorFixPlan,
} from './ip-access'

/** Saves attempted before giving up on a list that keeps changing. */
const MAX_ATTEMPTS = 3

export interface AllowMonitorTarget {
  projectId: number
  /** The environment whose own list refuses the monitor, else `null`. */
  environmentId: number | null
}

export type AllowMonitorOutcome =
  | { kind: 'saved'; saved: ProjectIpAccessResponse }
  /**
   * The warning was about the project list an environment followed, but
   * the environment has a list of its own now, which is what governs it.
   */
  | { kind: 'environment-list' }
  | Exclude<MonitorFixPlan, { kind: 'save' }>

/** The list the warning is about: the environment's own, or the project's. */
export function allowMonitorTarget(
  block: Pick<
    UptimeMonitorIpBlock,
    'project_id' | 'policy_scope' | 'environment_id'
  >
): AllowMonitorTarget {
  return {
    projectId: block.project_id,
    environmentId:
      block.policy_scope === 'environment'
        ? (block.environment_id ?? null)
        : null,
  }
}

/** How the action reads and replaces a list (the SDK, or a test's fake). */
export interface PolicyIo {
  read(target: AllowMonitorTarget): Promise<ProjectIpAccessResponse>
  write(
    target: AllowMonitorTarget,
    body: SetProjectIpAccessRequest
  ): Promise<ProjectIpAccessResponse>
}

async function read(
  target: AllowMonitorTarget
): Promise<ProjectIpAccessResponse> {
  const { projectId, environmentId } = target
  const result =
    environmentId != null
      ? await getEnvironmentIpAccess({
          path: { project_id: projectId, environment_id: environmentId },
          throwOnError: true,
        })
      : await getProjectIpAccess({
          path: { project_id: projectId },
          throwOnError: true,
        })
  return result.data
}

async function write(
  target: AllowMonitorTarget,
  body: SetProjectIpAccessRequest
): Promise<ProjectIpAccessResponse> {
  const { projectId, environmentId } = target
  const result =
    environmentId != null
      ? await putEnvironmentIpAccess({
          path: { project_id: projectId, environment_id: environmentId },
          body,
          throwOnError: true,
        })
      : await putProjectIpAccess({
          path: { project_id: projectId },
          body,
          throwOnError: true,
        })
  return result.data
}

const sdk: PolicyIo = { read, write }

/**
 * Re-read the list, check it still refuses the monitor the way `warning`
 * says, and save it with the monitor's entries added. Errors (a lockout
 * refusal included) are thrown for the caller to handle.
 */
export async function allowMonitor(
  warning: UptimeMonitorIpBlock,
  acknowledgeLockout: boolean,
  io: PolicyIo = sdk
): Promise<AllowMonitorOutcome> {
  const target = allowMonitorTarget(warning)
  // A warning about the project list, raised for one environment, applies
  // only while that environment still follows the project list. Adding the
  // monitor to the project list otherwise widens it for every environment
  // that inherits it, and does nothing for the one that warned.
  const inheritingEnvironment =
    warning.policy_scope === 'project' ? (warning.environment_id ?? null) : null
  for (let attempt = 1; ; attempt++) {
    if (inheritingEnvironment !== null) {
      const own = await io.read({
        projectId: warning.project_id,
        environmentId: inheritingEnvironment,
      })
      if (own.configured) return { kind: 'environment-list' }
    }
    const plan = planMonitorFix(warning, await io.read(target))
    if (plan.kind !== 'save') return plan
    try {
      const saved = await io.write(target, {
        mode: 'allow',
        cidrs: plan.cidrs.map((entry) => ({
          cidr: entry.cidr,
          description: entry.description ?? null,
        })),
        acknowledge_lockout: acknowledgeLockout,
        expected_revision: plan.revision,
      })
      return { kind: 'saved', saved }
    } catch (error) {
      if (attempt < MAX_ATTEMPTS && isRevisionConflict(error)) continue
      throw error
    }
  }
}

/** What to tell the operator about an outcome that saved nothing. */
export function allowMonitorNotice(
  outcome: Exclude<AllowMonitorOutcome, { kind: 'saved' }>
): string {
  switch (outcome.kind) {
    case 'environment-list':
      return 'This environment has its own IP access list now, and that list governs the monitor. Review it before allowing the monitor.'
    case 'already-allowed':
      return 'The IP access list already lets the monitor through.'
    case 'changed':
      return 'The IP access list changed since this warning was shown. Review it before allowing the monitor.'
    case 'full':
      return `The list holds at most ${outcome.max} entries. Remove ${outcome.needed - outcome.max} to make room for the monitor.`
  }
}
