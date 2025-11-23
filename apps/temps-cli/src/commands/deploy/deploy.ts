import { requireAuth, config } from '../../config/store.js'
import { promptSelect, promptText, promptConfirm } from '../../ui/prompts.js'
import { startSpinner, succeedSpinner, failSpinner, updateSpinner } from '../../ui/spinner.js'
import { success, info, warning, newline, icons, colors, header, keyValue, box } from '../../ui/output.js'
import { getClient } from '../../api/client.js'

interface DeployOptions {
  environment: string
  branch?: string
  wait?: boolean
}

interface Deployment {
  id: number
  status: string
  commit_sha?: string
  branch?: string
  created_at: string
}

export async function deploy(project: string | undefined, options: DeployOptions): Promise<void> {
  await requireAuth()

  newline()

  // Get project name
  const projectName = project ?? config.get('defaultProject')

  if (!projectName) {
    warning('No project specified')
    info('Use: temps deploy <project> or set a default with temps configure')
    return
  }

  const client = getClient()

  // Fetch project details
  startSpinner('Fetching project details...')

  let projectData: { id: number; name: string; environments?: Array<{ id: number; name: string }> }

  try {
    const response = await client.get('/api/projects/by-name/{name}' as '/api/projects/{id}', {
      params: { path: { name: projectName } as never },
    })

    if (response.error || !response.data) {
      failSpinner(`Project "${projectName}" not found`)
      return
    }

    projectData = response.data as typeof projectData
    succeedSpinner(`Found project: ${projectData.name}`)
  } catch (err) {
    failSpinner(`Failed to fetch project`)
    throw err
  }

  // Get environment
  let environmentName = options.environment

  if (projectData.environments && projectData.environments.length > 0) {
    if (!options.environment || options.environment === 'production') {
      environmentName = await promptSelect({
        message: 'Select environment',
        choices: projectData.environments.map((env) => ({
          name: env.name,
          value: env.name,
          description: env.name === 'production' ? 'Production environment' : undefined,
        })),
        default: 'production',
      })
    }
  }

  // Get branch if not specified
  const branch =
    options.branch ??
    (await promptText({
      message: 'Branch to deploy',
      default: 'main',
    }))

  // Confirm deployment
  newline()
  box(
    `Project: ${colors.bold(projectName)}\n` +
      `Environment: ${colors.bold(environmentName)}\n` +
      `Branch: ${colors.bold(branch)}`,
    `${icons.rocket} Deployment Preview`
  )
  newline()

  const confirmed = await promptConfirm({
    message: 'Start deployment?',
    default: true,
  })

  if (!confirmed) {
    info('Deployment cancelled')
    return
  }

  // Start deployment
  startSpinner('Starting deployment...')

  try {
    const response = await client.post('/api/deployments' as never, {
      body: {
        project_id: projectData.id,
        environment: environmentName,
        branch,
      },
    })

    if (response.error || !response.data) {
      failSpinner('Failed to start deployment')
      return
    }

    const deployment = response.data as Deployment
    succeedSpinner(`Deployment #${deployment.id} started`)

    if (options.wait !== false) {
      await waitForDeployment(client, deployment.id)
    } else {
      newline()
      info(`Deployment running in background`)
      info(`Check status with: temps deployments status ${deployment.id}`)
    }
  } catch (err) {
    failSpinner('Deployment failed')
    throw err
  }
}

async function waitForDeployment(client: ReturnType<typeof getClient>, deploymentId: number): Promise<void> {
  const statusMessages: Record<string, string> = {
    pending: 'Waiting in queue...',
    building: 'Building application...',
    deploying: 'Deploying to servers...',
    running: 'Starting containers...',
  }

  startSpinner('Waiting for deployment...')

  let lastStatus = ''

  // eslint-disable-next-line no-constant-condition
  while (true) {
    const response = await client.get('/api/deployments/{id}' as never, {
      params: { path: { id: deploymentId } },
    })

    if (response.error) {
      failSpinner('Failed to check deployment status')
      return
    }

    const deployment = response.data as Deployment

    if (deployment.status !== lastStatus) {
      lastStatus = deployment.status
      updateSpinner(statusMessages[deployment.status] ?? `Status: ${deployment.status}`)
    }

    if (deployment.status === 'success' || deployment.status === 'completed') {
      succeedSpinner(`${icons.rocket} Deployment successful!`)
      newline()
      header(`${icons.check} Deployment Complete`)
      keyValue('Deployment ID', deployment.id)
      keyValue('Commit', deployment.commit_sha?.substring(0, 7))
      keyValue('Duration', 'calculating...')
      newline()
      return
    }

    if (deployment.status === 'failed' || deployment.status === 'error') {
      failSpinner('Deployment failed')
      newline()
      info(`View logs with: temps logs ${deploymentId}`)
      return
    }

    // Wait before checking again
    await new Promise((resolve) => setTimeout(resolve, 2000))
  }
}
