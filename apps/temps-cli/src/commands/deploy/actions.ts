// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  cancelDeployment,
  pauseDeployment,
  resumeDeployment,
  teardownDeployment,
  getDeployment,
} from '../../api/sdk.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { promptConfirm } from '../../ui/prompts.js'
import { newline, success, info, warning, keyValue } from '../../ui/output.js'

interface DeploymentActionOptions {
  projectId: string
  deploymentId: string
  force?: boolean
}

/**
 * Parses the `--project-id`/`--deployment-id` CLI flags into numeric IDs.
 * Returns null on anything non-numeric so callers can bail out with a clear
 * warning instead of sending `NaN` into an API path segment.
 */
export function parseDeploymentTarget(
  options: { projectId: string; deploymentId: string }
): { projectId: number; deploymentId: number } | null {
  const projectId = parseInt(options.projectId, 10)
  const deploymentId = parseInt(options.deploymentId, 10)
  if (isNaN(projectId) || isNaN(deploymentId)) {
    return null
  }
  return { projectId, deploymentId }
}

export async function cancelDeploymentAction(
  options: DeploymentActionOptions
): Promise<void> {
  await requireAuth()
  await setupClient()

  const target = parseDeploymentTarget(options)
  if (!target) {
    warning('Invalid project or deployment ID')
    return
  }
  const { projectId: projId, deploymentId: deplId } = target

  if (!options.force) {
    const confirmed = await promptConfirm({
      message: `Cancel deployment #${options.deploymentId}?`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  await withSpinner('Cancelling deployment...', async () => {
    const { error } = await cancelDeployment({
      client,
      path: { project_id: projId, deployment_id: deplId },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success(`Deployment #${options.deploymentId} cancelled`)
}

export async function pauseDeploymentAction(
  options: { projectId: string; deploymentId: string }
): Promise<void> {
  await requireAuth()
  await setupClient()

  const target = parseDeploymentTarget(options)
  if (!target) {
    warning('Invalid project or deployment ID')
    return
  }
  const { projectId: projId, deploymentId: deplId } = target

  await withSpinner('Pausing deployment...', async () => {
    const { error } = await pauseDeployment({
      client,
      path: { project_id: projId, deployment_id: deplId },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success(`Deployment #${options.deploymentId} paused`)
  info('Use "temps deployments resume" to resume')
}

export async function resumeDeploymentAction(
  options: { projectId: string; deploymentId: string }
): Promise<void> {
  await requireAuth()
  await setupClient()

  const target = parseDeploymentTarget(options)
  if (!target) {
    warning('Invalid project or deployment ID')
    return
  }
  const { projectId: projId, deploymentId: deplId } = target

  await withSpinner('Resuming deployment...', async () => {
    const { error } = await resumeDeployment({
      client,
      path: { project_id: projId, deployment_id: deplId },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success(`Deployment #${options.deploymentId} resumed`)
}

export async function teardownDeploymentAction(
  options: DeploymentActionOptions
): Promise<void> {
  await requireAuth()
  await setupClient()

  const target = parseDeploymentTarget(options)
  if (!target) {
    warning('Invalid project or deployment ID')
    return
  }
  const { projectId: projId, deploymentId: deplId } = target

  // Get deployment info first
  const deployment = await withSpinner('Fetching deployment...', async () => {
    const { data, error } = await getDeployment({
      client,
      path: { project_id: projId, deployment_id: deplId },
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error) ?? 'Deployment not found')
    }
    return data
  })

  if (!options.force) {
    newline()
    warning('This will remove all containers and resources for this deployment!')
    keyValue('Deployment', `#${deployment.id}`)
    keyValue('Environment', deployment.environment.name)
    keyValue('Status', deployment.status)
    newline()

    const confirmed = await promptConfirm({
      message: 'Are you sure you want to teardown this deployment?',
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  await withSpinner('Tearing down deployment...', async () => {
    const { error } = await teardownDeployment({
      client,
      path: { project_id: projId, deployment_id: deplId },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success(`Deployment #${options.deploymentId} has been torn down`)
  info('All containers and resources have been removed')
}
