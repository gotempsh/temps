// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { requireProjectSlug } from '../../config/resolve-project.js'
import {
  getProjectBySlug,
  getProjectDeployments,
  rollbackToDeployment,
} from '../../api/sdk.gen.js'
import type { DeploymentResponse } from '../../api/types.gen.js'
import { promptConfirm, promptSelect } from '../../ui/prompts.js'
import { withSpinner } from '../../ui/spinner.js'
import { warning, newline, colors, info, icons, header, keyValue } from '../../ui/output.js'

interface RollbackOptions {
  project?: string
  environment: string
  to?: string
}

/**
 * Only deployments that finished successfully in the target environment are
 * valid rollback targets — rolling back to a failed or in-progress deployment
 * would redeploy broken state. Capped to the 10 most recent so the picker
 * stays scannable.
 */
export function filterRollbackCandidates(
  deployments: DeploymentResponse[],
  environment: string
): DeploymentResponse[] {
  return deployments
    .filter(
      (d) =>
        d.environment?.name === environment &&
        (d.status === 'success' || d.status === 'completed' || d.status === 'deployed')
    )
    .slice(0, 10)
}

export function formatRollbackChoice(d: DeploymentResponse): {
  name: string
  value: string
  description: string
} {
  const isRollback = d.metadata?.isRollback
  const branch = d.branch ?? (isRollback ? 'rollback' : 'unknown')
  const commit =
    d.commit_hash?.substring(0, 7) ?? (isRollback ? `from #${d.metadata?.rolledBackFromId ?? '?'}` : '-')
  const currentTag = d.is_current ? ' (current)' : ''
  const date = new Date(d.created_at * 1000).toLocaleString()

  return {
    name: `#${d.id} - ${branch} (${commit})${currentTag}`,
    value: String(d.id),
    description: date,
  }
}

export async function rollback(options: RollbackOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const resolved = await requireProjectSlug(options.project)

  if (resolved.source !== 'flag') {
    info(`Using project ${colors.bold(resolved.slug)} (from ${resolved.source})`)
  }

  newline()
  warning(`Rolling back ${colors.bold(resolved.slug)} in ${colors.bold(options.environment)}`)
  newline()

  // Get project ID
  const { data: projectData, error: projectError } = await getProjectBySlug({
    client,
    path: { slug: resolved.slug },
  })

  if (projectError || !projectData) {
    throw new Error(`Project "${resolved.slug}" not found`)
  }

  let targetDeploymentId = options.to ? parseInt(options.to, 10) : undefined

  if (!targetDeploymentId) {
    // Fetch recent successful deployments
    const deployments = await withSpinner('Fetching deployment history...', async () => {
      const { data, error } = await getProjectDeployments({
        client,
        path: { id: projectData.id },
      })

      if (error || !data) {
        throw new Error(getErrorMessage(error))
      }

      // Filter by environment and completed status
      return filterRollbackCandidates(data.deployments, options.environment)
    })

    if (deployments.length === 0) {
      warning('No completed deployments found for this environment')
      return
    }

    // Show all deployments, mark which is current
    const selectedId = await promptSelect({
      message: 'Select deployment to rollback to',
      choices: deployments.map(formatRollbackChoice),
    })

    targetDeploymentId = parseInt(selectedId, 10)
  }

  const confirmed = await promptConfirm({
    message: `Rollback to deployment #${targetDeploymentId}?`,
    default: false,
  })

  if (!confirmed) {
    info('Rollback cancelled')
    return
  }

  const newDeployment = await withSpinner('Initiating rollback...', async () => {
    const { data, error } = await rollbackToDeployment({
      client,
      path: {
        project_id: projectData.id,
        deployment_id: targetDeploymentId!,
      },
    })

    if (error || !data) {
      throw new Error(getErrorMessage(error) ?? 'Failed to initiate rollback')
    }

    return data
  })

  newline()
  header(`${icons.check} Rollback Initiated`)
  keyValue('New Deployment ID', newDeployment.id)
  keyValue('Status', newDeployment.status)
  newline()

  info(`Track progress with: temps deployments status --project ${resolved.slug} --deployment-id ${newDeployment.id}`)
}
