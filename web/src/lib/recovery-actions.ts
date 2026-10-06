// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * One-click remedies offered wherever a failure is shown (project overview,
 * alarms): redeploy a deployment from the same source, roll back to an older
 * deployment, or restart a container. Each maps onto an API the console
 * already uses elsewhere; this module only describes and runs them, so the
 * confirmation dialog and its tests share one definition.
 */

import type { SourceType } from '@/api/client'
import { restartContainer, rollbackToDeployment } from '@/api/client/sdk.gen'
import {
  redeployDeployment,
  sdkRedeployApi,
  type RedeployApi,
} from '@/lib/service-link-redeploy'

export type RecoveryAction =
  | {
      kind: 'redeploy'
      projectId: number
      sourceType: SourceType
      deploymentId: number
    }
  | {
      kind: 'rollback'
      projectId: number
      targetDeploymentId: number
      /** The target is still serving traffic in its environment. */
      targetIsLive: boolean
    }
  | {
      kind: 'restart_container'
      projectId: number
      environmentId: number
      /** Docker container ID. */
      containerId: string
      containerName: string
    }

export type RecoveryActionCopy = {
  title: string
  description: string
  confirmLabel: string
  /** Toast shown once the API accepted the action. */
  success: string
  /** Title of the error toast if it fails. */
  errorTitle: string
}

export function recoveryActionCopy(action: RecoveryAction): RecoveryActionCopy {
  switch (action.kind) {
    case 'redeploy':
      return {
        title: `Redeploy #${action.deploymentId}?`,
        description:
          'Starts a new deployment from the same source (commit, image or static bundle) with the current environment variables. Whatever is live keeps serving until the new deployment is ready.',
        confirmLabel: 'Redeploy',
        success: `Redeploy of #${action.deploymentId} started`,
        errorTitle: `Failed to redeploy #${action.deploymentId}`,
      }
    case 'rollback':
      return {
        title: `Roll back to #${action.targetDeploymentId}?`,
        description: action.targetIsLive
          ? `#${action.targetDeploymentId} is still serving traffic. Rolling back starts a fresh deployment of it, which becomes the latest deployment again.`
          : `Starts a new deployment from #${action.targetDeploymentId}'s image and switches traffic to it once it is ready.`,
        confirmLabel: 'Roll back',
        success: `Rollback to #${action.targetDeploymentId} started`,
        errorTitle: `Failed to roll back to #${action.targetDeploymentId}`,
      }
    case 'restart_container':
      return {
        title: `Restart ${action.containerName}?`,
        description:
          'Restarts the container in place. Requests to it fail briefly while it comes back up.',
        confirmLabel: 'Restart',
        success: `${action.containerName} restarted`,
        errorTitle: `Failed to restart ${action.containerName}`,
      }
  }
}

/** The API calls a recovery action makes; injectable for tests. */
export type RecoveryApi = RedeployApi & {
  rollbackToDeployment: typeof rollbackToDeployment
  restartContainer: typeof restartContainer
}

const sdkRecoveryApi: RecoveryApi = {
  ...sdkRedeployApi,
  rollbackToDeployment,
  restartContainer,
}

/** Run a recovery action; rejects with a contextual error if it can't. */
export async function runRecoveryAction(
  action: RecoveryAction,
  api: RecoveryApi = sdkRecoveryApi
): Promise<void> {
  switch (action.kind) {
    case 'redeploy': {
      const { data: deployment } = await api.getDeployment({
        path: {
          project_id: action.projectId,
          deployment_id: action.deploymentId,
        },
        throwOnError: true,
      })
      const skipped = await redeployDeployment(
        api,
        action.projectId,
        action.sourceType,
        deployment
      )
      if (skipped) {
        throw new Error(
          `Deployment #${action.deploymentId} can't be redeployed: ${skipped}`
        )
      }
      return
    }
    case 'rollback':
      await api.rollbackToDeployment({
        path: {
          project_id: action.projectId,
          deployment_id: action.targetDeploymentId,
        },
        throwOnError: true,
      })
      return
    case 'restart_container':
      await api.restartContainer({
        path: {
          project_id: action.projectId,
          environment_id: action.environmentId,
          container_id: action.containerId,
        },
        throwOnError: true,
      })
      return
  }
}

/**
 * Generated query IDs whose data a recovery action changes; the dialog
 * invalidates these so status badges and banners update without a reload.
 */
export const RECOVERY_AFFECTED_QUERY_IDS = new Set([
  'getLastDeployment',
  'getEnvironments',
  'getProjectDeployments',
  'getDeployment',
  'listContainers',
  'getContainerDetail',
  'listContainerHistory',
  'listProjectAlarms',
  'getProjectAlarmsSummary',
])
