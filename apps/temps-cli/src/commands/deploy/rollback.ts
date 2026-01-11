import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  getProjectBySlug,
  getProjectDeployments,
  rollbackToDeployment,
} from '../../api/sdk.gen.js'
import { promptConfirm, promptSelect } from '../../ui/prompts.js'
import { withSpinner } from '../../ui/spinner.js'
import { success, warning, newline, colors, info, icons, header, keyValue } from '../../ui/output.js'

interface RollbackOptions {
  project?: string
  environment: string
  to?: string
}

export async function rollback(options: RollbackOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  if (!options.project) {
    throw new Error('Project is required. Use: temps deployments rollback --project <project>')
  }

  newline()
  warning(`Rolling back ${colors.bold(options.project)} in ${colors.bold(options.environment)}`)
  newline()

  // Get project ID
  const { data: projectData, error: projectError } = await getProjectBySlug({
    client,
    path: { slug: options.project },
  })

  if (projectError || !projectData) {
    throw new Error(`Project "${options.project}" not found`)
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

      // Filter by environment and status
      return data.deployments
        .filter(d =>
          d.environment?.name === options.environment &&
          (d.status === 'success' || d.status === 'completed' || d.status === 'deployed')
        )
        .slice(0, 5)
    })

    if (deployments.length < 2) {
      warning('No previous deployments to rollback to')
      return
    }

    // Skip current, show previous deployments
    const previousDeployments = deployments.slice(1)

    const selectedId = await promptSelect({
      message: 'Select deployment to rollback to',
      choices: previousDeployments.map((d) => ({
        name: `#${d.id} - ${d.branch ?? 'unknown'} (${d.commit_hash?.substring(0, 7) ?? 'unknown'})`,
        value: String(d.id),
        description: new Date(d.created_at * 1000).toLocaleString(),
      })),
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

  info(`Track progress with: temps deployments status --project ${options.project} --deployment-id ${newDeployment.id}`)
}
