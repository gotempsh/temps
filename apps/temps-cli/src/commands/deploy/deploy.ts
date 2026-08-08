import { requireAuth, config } from '../../config/store.js'
import { setupClient, client, getErrorMessage, getWebUrl } from '../../lib/api-client.js'
import { resolveProjectSlug } from '../../config/resolve-project.js'
import {
  getProjectBySlug,
  getEnvironments,
  triggerProjectPipeline,
  getProjectDeployments,
  getRepositoryByName,
} from '../../api/sdk.gen.js'
import type { EnvironmentResponse, ProjectResponse } from '../../api/types.gen.js'
import { promptSelect, promptText } from '../../ui/prompts.js'
import { startSpinner, succeedSpinner, failSpinner } from '../../ui/spinner.js'
import { info, warning, newline, icons, colors, box } from '../../ui/output.js'
import { watchDeployment } from '../../lib/deployment-watcher.jsx'
import { deployLocalImage } from './deploy-local-image.js'
import { deployStatic } from './deploy-static.js'

// Types for the /repository/{id}/commits endpoint (not yet in generated SDK)
interface CommitInfo {
  sha: string
  message: string
  author: string
  author_email: string
  date: string
}

interface CommitListResponse {
  commits: CommitInfo[]
}

/**
 * Fetch recent commits for a repository branch from the remote git provider.
 */
async function fetchRemoteCommits(
  repositoryId: number,
  branch: string,
  perPage = 20,
): Promise<CommitInfo[]> {
  try {
    const response = await client.get({
      security: [{ scheme: 'bearer', type: 'http' }],
      url: '/repository/{repository_id}/commits',
      path: { repository_id: repositoryId },
      query: { branch, per_page: perPage },
    })
    const data = response.data as CommitListResponse | undefined
    return data?.commits ?? []
  } catch {
    return []
  }
}

/**
 * Look up the repository ID for a project's repo_owner/repo_name.
 */
async function getRepositoryId(
  repoOwner: string,
  repoName: string,
  connectionId?: number | null,
): Promise<number | null> {
  try {
    const { data } = await getRepositoryByName({
      client,
      path: { owner: repoOwner, name: repoName },
      query: connectionId ? { connection_id: connectionId } : undefined,
    })
    return data?.id ?? null
  } catch {
    return null
  }
}

export function getRelativeTime(date: Date): string {
  const seconds = Math.floor((Date.now() - date.getTime()) / 1000)
  if (seconds < 60) return 'just now'
  const minutes = Math.floor(seconds / 60)
  if (minutes < 60) return `${minutes}m ago`
  const hours = Math.floor(minutes / 60)
  if (hours < 24) return `${hours}h ago`
  const days = Math.floor(hours / 24)
  return `${days}d ago`
}

interface DeployOptions {
  project?: string
  environment?: string
  environmentId?: string
  branch?: string
  commit?: string
  wait?: boolean
  yes?: boolean
}

export async function deploy(options: DeployOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  newline()

  // Resolve project: --project flag > .temps/config.json > TEMPS_PROJECT env > global default
  const resolved = await resolveProjectSlug(options.project)

  if (!resolved) {
    warning('No project specified')
    info('Use: bunx @temps-sdk/cli deploy --project <slug>')
    info('Or link this directory: bunx @temps-sdk/cli link <slug>')
    return
  }

  const projectName = resolved.slug

  if (resolved.source !== 'flag') {
    info(`Using project ${colors.bold(projectName)} (from ${resolved.source})`)
  }

  // Fetch project details
  startSpinner('Fetching project details...')

  let project: ProjectResponse
  let environments: EnvironmentResponse[] = []

  try {
    const { data, error } = await getProjectBySlug({
      client,
      path: { slug: projectName },
    })

    if (error || !data) {
      const rawApiUrl = config.get('apiUrl')
      const baseUrl = client.getConfig().baseUrl ?? rawApiUrl
      failSpinner(`Project "${projectName}" not found`)
      info(`API: ${colors.muted(`${baseUrl}/projects/by-slug/${projectName}`)}`)
      if (error) {
        info(`Error: ${getErrorMessage(error)}`)
      }
      return
    }

    project = data
    succeedSpinner(`Found project: ${project.name}`)

    // Fetch environments
    const { data: envData } = await getEnvironments({
      client,
      path: { project_id: project.id },
    })
    environments = envData ?? []
  } catch (err) {
    failSpinner('Failed to fetch project')
    throw err
  }

  // Check source type — delegate to appropriate deploy method
  const sourceType = project.source_type
  const isGitBased = sourceType === 'git'
  const hasGitConnection = isGitBased && !!project.git_provider_connection_id

  if (sourceType === 'static_files') {
    info(`Project uses static files deployment`)
    info(`Delegating to: ${colors.muted('deploy:static')}`)
    newline()
    await deployStatic({
      path: '.',
      project: projectName,
      environment: options.environment,
      environmentId: options.environmentId,
      wait: options.wait,
      yes: options.yes,
    })
    return
  }

  if (sourceType === 'docker_image' || sourceType === 'manual') {
    info(`Project uses ${sourceType === 'docker_image' ? 'Docker image' : 'manual'} deployment`)
    info(`Delegating to: ${colors.muted('deploy:local-image')}`)
    newline()
    await deployLocalImage({
      project: projectName,
      environment: options.environment,
      environmentId: options.environmentId,
      wait: options.wait,
      yes: options.yes,
    })
    return
  }

  // Git-based project — check if git is actually connected
  if (!hasGitConnection) {
    warning('Project is git-based but no git provider is connected')
    newline()
    info('Options:')
    info(`  1. Connect a git provider: ${colors.muted('bunx @temps-sdk/cli providers add')}`)
    info(`  2. Deploy a local Docker image: ${colors.muted(`bunx @temps-sdk/cli deploy:local-image -p ${projectName}`)}`)
    info(`  3. Deploy static files: ${colors.muted(`bunx @temps-sdk/cli deploy:static -p ${projectName} --path ./dist`)}`)
    newline()

    if (!options.yes) {
      const choice = await promptSelect({
        message: 'How would you like to deploy?',
        choices: [
          { name: 'Build & deploy local Docker image', value: 'local-image' },
          { name: 'Deploy static files', value: 'static' },
          { name: 'Cancel', value: 'cancel' },
        ],
      })

      if (choice === 'local-image') {
        await deployLocalImage({
          project: projectName,
          environment: options.environment,
          environmentId: options.environmentId,
          wait: options.wait,
          yes: options.yes,
        })
        return
      }

      if (choice === 'static') {
        const staticPath = await promptText({
          message: 'Path to static files',
          default: './dist',
        })
        await deployStatic({
          path: staticPath,
          project: projectName,
          environment: options.environment,
          environmentId: options.environmentId,
          wait: options.wait,
          yes: options.yes,
        })
        return
      }

      // Cancel
      return
    }

    // Non-interactive with --yes: fall back to local-image
    info('Falling back to local Docker image deployment (--yes mode)')
    await deployLocalImage({
      project: projectName,
      environment: options.environment,
      environmentId: options.environmentId,
      wait: options.wait,
      yes: options.yes,
    })
    return
  }

  // ─── Git-based deployment with connected provider ───────────────────────

  if (project.repo_owner && project.repo_name) {
    info(`Repository: ${colors.muted(`${project.repo_owner}/${project.repo_name}`)}`)
  }

  // Get environment
  let environmentId: number | undefined
  let environmentName = options.environment || 'production'

  if (environments.length > 0) {
    if (options.environmentId) {
      environmentId = parseInt(options.environmentId, 10)
      const env = environments.find(e => e.id === environmentId)
      if (env) {
        environmentName = env.name
      }
    } else if (options.environment) {
      const env = environments.find(e => e.name === options.environment)
      if (env) {
        environmentId = env.id
        environmentName = env.name
      }
    } else if (!options.yes) {
      const selectedEnv = await promptSelect({
        message: 'Select environment',
        choices: environments.map((env) => ({
          name: env.name,
          value: String(env.id),
          description: env.is_preview ? 'Preview environment' : undefined,
        })),
        default: String(environments.find(e => e.name === 'production')?.id ?? environments[0]?.id ?? ''),
      })
      environmentId = parseInt(selectedEnv, 10)
      environmentName = environments.find(e => e.id === environmentId)?.name ?? 'production'
    } else {
      const prodEnv = environments.find(e => e.name === 'production')
      if (prodEnv) {
        environmentId = prodEnv.id
        environmentName = prodEnv.name
      } else if (environments[0]) {
        environmentId = environments[0].id
        environmentName = environments[0].name
      }
    }
  }

  // Get branch
  let branch = options.branch
  if (!branch) {
    if (options.yes) {
      branch = project.main_branch || 'main'
    } else {
      branch = await promptText({
        message: 'Branch to deploy',
        default: project.main_branch || 'main',
      })
    }
  }

  // Select commit — resolve from flag, interactive picker, or skip (deploy HEAD)
  let commit = options.commit
  if (!commit && !options.yes && project.repo_owner && project.repo_name) {
    // Try to fetch recent commits so the user can pick one
    const repositoryId = await getRepositoryId(
      project.repo_owner,
      project.repo_name,
      project.git_provider_connection_id,
    )

    if (repositoryId) {
      startSpinner('Fetching recent commits...')
      const commits = await fetchRemoteCommits(repositoryId, branch)
      if (commits.length > 0) {
        succeedSpinner(`Found ${commits.length} recent commits`)

        const HEAD_VALUE = '__HEAD__'
        const selected = await promptSelect({
          message: 'Select commit to deploy',
          choices: [
            { name: `${colors.bold('HEAD')} ${colors.muted('(latest on branch)')}`, value: HEAD_VALUE },
            ...commits.map(c => {
              const sha = colors.muted(c.sha.substring(0, 7))
              const msg = (c.message.split('\n')[0] ?? '').substring(0, 60)
              const ago = getRelativeTime(new Date(c.date))
              return {
                name: `${sha} ${msg} ${colors.muted(`(${c.author}, ${ago})`)}`,
                value: c.sha,
              }
            }),
          ],
        })
        if (selected !== HEAD_VALUE) {
          commit = selected
        }
      } else {
        succeedSpinner('No commits found, deploying HEAD')
      }
    }
  }

  // Deployment preview
  newline()
  box(
    [
      `Project:     ${colors.bold(projectName)}`,
      `Environment: ${colors.bold(environmentName)}`,
      `Branch:      ${colors.bold(branch)}`,
      commit ? `Commit:      ${colors.bold(commit.substring(0, 7))}` : null,
      project.preset ? `Preset:      ${colors.bold(project.preset)}` : null,
      project.repo_owner && project.repo_name
        ? `Repository:  ${colors.bold(`${project.repo_owner}/${project.repo_name}`)}`
        : null,
    ]
      .filter(Boolean)
      .join('\n'),
    `${icons.rocket} Deployment Preview`
  )
  newline()

  // Trigger git-based deployment
  startSpinner('Starting deployment...')

  try {
    const { data, error } = await triggerProjectPipeline({
      client,
      path: { id: project.id },
      body: {
        branch,
        commit: commit ?? undefined,
        environment_id: environmentId,
      },
    })

    if (error || !data) {
      failSpinner('Failed to start deployment')
      const msg = getErrorMessage(error)
      if (msg) {
        info(`Reason: ${msg}`)
      }
      return
    }

    succeedSpinner('Deployment started')
    info(data.message ?? 'Pipeline triggered successfully')

    const webUrl = getWebUrl()
    info(`Dashboard: ${colors.primary(`${webUrl}/projects/${projectName}/deployments`)}`)

    if (options.wait === false) {
      return
    }

    // Find the deployment ID so we can watch it with the rich TUI
    startSpinner('Waiting for deployment to start...')
    let deploymentId: number | null = null

    for (let attempt = 0; attempt < 15; attempt++) {
      const { data: deployList, error: deployError } = await getProjectDeployments({
        client,
        path: { id: project.id },
        query: {
          per_page: 1,
          ...(environmentId ? { environment_id: environmentId } : {}),
        },
      })

      if (deployError) {
        failSpinner('Failed to fetch deployment status')
        info(`Error: ${getErrorMessage(deployError)}`)
        info(`Dashboard: ${colors.primary(`${webUrl}/projects/${projectName}/deployments`)}`)
        return
      }

      const latest = deployList?.deployments?.[0]
      if (latest?.id) {
        deploymentId = latest.id
        break
      }

      await new Promise((r) => setTimeout(r, 2000))
    }

    if (deploymentId) {
      succeedSpinner(`Deployment #${deploymentId} found`)
      const result = await watchDeployment({
        projectId: project.id,
        deploymentId,
        timeoutSecs: 600,
        projectName,
      })

      if (!result.success) {
        process.exitCode = 1
      }
    } else {
      failSpinner('Could not locate the deployment to track')
      info(`Dashboard: ${colors.primary(`${webUrl}/projects/${projectName}/deployments`)}`)
    }
  } catch (err) {
    failSpinner('Deployment failed')
    throw err
  }
}
