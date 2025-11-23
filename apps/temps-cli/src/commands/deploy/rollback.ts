import { requireAuth } from '../../config/store.js'
import { promptConfirm, promptSelect } from '../../ui/prompts.js'
import { withSpinner } from '../../ui/spinner.js'
import { success, warning, newline, colors, info, icons, header, keyValue } from '../../ui/output.js'
import { getClient } from '../../api/client.js'

interface RollbackOptions {
  environment: string
  to?: string
}

interface Deployment {
  id: number
  status: string
  branch?: string
  commit_sha?: string
  created_at: string
}

export async function rollback(project: string, options: RollbackOptions): Promise<void> {
  await requireAuth()

  const client = getClient()

  newline()
  warning(`Rolling back ${colors.bold(project)} in ${colors.bold(options.environment)}`)
  newline()

  let targetDeploymentId = options.to

  if (!targetDeploymentId) {
    // Fetch recent successful deployments
    const deployments = await withSpinner('Fetching deployment history...', async () => {
      const response = await client.get('/api/projects/{project}/deployments' as never, {
        params: {
          path: { project },
          query: {
            environment: options.environment,
            status: 'success',
            limit: 5,
          },
        },
      })

      if (response.error) {
        throw new Error('Failed to fetch deployments')
      }

      return (response.data ?? []) as Deployment[]
    })

    if (deployments.length < 2) {
      warning('No previous deployments to rollback to')
      return
    }

    // Skip current, show previous deployments
    const previousDeployments = deployments.slice(1)

    targetDeploymentId = await promptSelect({
      message: 'Select deployment to rollback to',
      choices: previousDeployments.map((d) => ({
        name: `#${d.id} - ${d.branch ?? 'unknown'} (${d.commit_sha?.substring(0, 7) ?? 'unknown'})`,
        value: String(d.id),
        description: new Date(d.created_at).toLocaleString(),
      })),
    })
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
    const response = await client.post('/api/deployments/{id}/rollback' as never, {
      params: { path: { id: targetDeploymentId } },
    })

    if (response.error || !response.data) {
      throw new Error('Failed to initiate rollback')
    }

    return response.data as Deployment
  })

  newline()
  header(`${icons.check} Rollback Initiated`)
  keyValue('New Deployment ID', newDeployment.id)
  keyValue('Status', newDeployment.status)
  newline()

  info(`Track progress with: temps deployments status ${newDeployment.id}`)
}
