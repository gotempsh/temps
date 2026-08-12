import type { Command } from 'commander'
import { requireAuth, config } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { resolveProjectSlug } from '../../config/resolve-project.js'
import { hasProjectConfig, writeProjectConfig } from '../../config/project-config.js'
import { deployLocalImage } from '../deploy/deploy-local-image.js'
import { runSetupWizard } from './setup-wizard.js'
import { detectGitBranch } from '../../lib/detect-project.js'
import { promptConfirm, promptSelect, promptText } from '../../ui/prompts.js'
import { startSpinner, succeedSpinner, failSpinner } from '../../ui/spinner.js'
import { info, warning, newline, colors, icons, box } from '../../ui/output.js'
import {
  getProjectBySlug,
  getEnvironments,
  triggerProjectPipeline,
  getProjectDeployments,
} from '../../api/sdk.gen.js'
import type { ProjectResponse, EnvironmentResponse } from '../../api/types.gen.js'
import { watchDeployment } from '../../lib/deployment-watcher.jsx'
import { waitForTriggeredDeployment } from '../../lib/find-triggered-deployment.js'

export interface EnvironmentSelection {
  environmentId: number | undefined
  environmentName: string
}

/**
 * Resolve `--environment <name>` against the project's real environments.
 * A typo/stale name must NOT silently fall back to some other environment —
 * it leaves environmentId undefined (so the caller can decide how to fail)
 * while environmentName still reflects what the user actually asked for.
 */
export function resolveExplicitEnvironment(
  environments: Pick<EnvironmentResponse, 'id' | 'name'>[],
  name: string
): EnvironmentSelection {
  const env = environments.find(e => e.name === name)
  return env
    ? { environmentId: env.id, environmentName: env.name }
    : { environmentId: undefined, environmentName: name }
}

/** Non-interactive (`--yes`) default: prefer `production`, else the first environment. */
export function resolveDefaultEnvironment(
  environments: Pick<EnvironmentResponse, 'id' | 'name'>[]
): EnvironmentSelection {
  const prodEnv = environments.find(e => e.name === 'production')
  if (prodEnv) return { environmentId: prodEnv.id, environmentName: prodEnv.name }
  if (environments[0]) return { environmentId: environments[0].id, environmentName: environments[0].name }
  return { environmentId: undefined, environmentName: 'production' }
}

interface UpOptions {
  project?: string
  environment?: string
  branch?: string
  name?: string
  preset?: string
  manual?: boolean
  static?: boolean
  staticDir?: string
  noServices?: boolean
  wait?: boolean
  yes?: boolean
}

async function up(projectArg: string | undefined, options: UpOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  // Resolve project — check if already linked
  const resolved = await resolveProjectSlug(projectArg ?? options.project)

  if (!resolved) {
    // No project linked — run the setup wizard
    const result = await runSetupWizard({
      name: options.name,
      preset: options.preset,
      branch: options.branch,
      manual: options.manual,
      static: options.static,
      staticDir: options.staticDir,
      noServices: options.noServices,
      yes: options.yes,
    })

    if (!result) {
      // Wizard was cancelled
      return
    }

    // Wizard already triggered the first deployment and saved config
    return
  }

  // ─── Project is already linked — deploy it ──────────────────────────────

  if (resolved.source !== 'flag') {
    info(`Using project ${colors.bold(resolved.slug)} (from ${resolved.source})`)
  }

  newline()

  // Fetch project details
  startSpinner('Fetching project details...')

  let project: ProjectResponse
  let environments: EnvironmentResponse[] = []

  try {
    const { data, error } = await getProjectBySlug({
      client,
      path: { slug: resolved.slug },
    })

    if (error || !data) {
      // The project didn't resolve to a real project on this server. When the
      // slug came from an explicit --project flag, that's a genuine error —
      // the user named something that doesn't exist. But when it came from a
      // saved default (context-default / global-config / local-config), it's
      // almost always a stale default left over from another instance, so
      // fall through to the setup wizard and offer to create one instead of
      // dead-ending on a 404.
      if (resolved.source === 'flag') {
        const rawApiUrl = config.get('apiUrl')
        const baseUrl = client.getConfig().baseUrl ?? rawApiUrl
        failSpinner(`Project "${resolved.slug}" not found`)
        info(`API: ${colors.muted(`${baseUrl}/projects/by-slug/${resolved.slug}`)}`)
        if (error) {
          info(`Error: ${getErrorMessage(error)}`)
        }
        return
      }

      failSpinner(`No project "${resolved.slug}" on this server (from ${resolved.source})`)
      info('Setting up a new project instead...')
      newline()

      const result = await runSetupWizard({
        name: options.name,
        preset: options.preset,
        branch: options.branch,
        manual: options.manual,
        noServices: options.noServices,
        yes: options.yes,
      })

      // Wizard (when not cancelled) already triggered the deploy and saved config.
      void result
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

  // Show project context
  const sourceType = project.source_type
  const isGitBased = sourceType !== 'manual' && sourceType !== 'docker_image'

  if (isGitBased && project.repo_owner && project.repo_name) {
    info(`Repository: ${colors.muted(`${project.repo_owner}/${project.repo_name}`)}`)
  } else if (isGitBased && !project.git_provider_connection_id) {
    warning('No git provider connected to this project')
    info(`Connect one with: ${colors.muted('bunx @temps-sdk/cli providers add')}`)
    info(`Or deploy manually: ${colors.muted(`bunx @temps-sdk/cli deploy:local-image -p ${resolved.slug}`)}`)
    newline()
  }

  if (project.preset) {
    info(`Preset: ${colors.muted(project.preset)}`)
  }

  // ─── Select environment ─────────────────────────────────────────────────

  let environmentId: number | undefined
  let environmentName = options.environment || 'production'

  if (environments.length > 0) {
    if (options.environment) {
      ;({ environmentId, environmentName } = resolveExplicitEnvironment(environments, options.environment))
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
      ;({ environmentId, environmentName } = resolveDefaultEnvironment(environments))
    }
  }

  // ─── Deploy based on source type ────────────────────────────────────────

  if (sourceType === 'manual' || sourceType === 'docker_image') {
    // Show deployment preview
    newline()
    box(
      [
        `Project:     ${colors.bold(project.name)}`,
        `Environment: ${colors.bold(environmentName)}`,
        project.preset ? `Preset:      ${colors.bold(project.preset)}` : null,
        `Deploy:      ${colors.bold('Manual (local image upload)')}`,
      ]
        .filter(Boolean)
        .join('\n'),
      `${icons.rocket} Deployment Preview`
    )
    newline()

    await deployLocalImage({
      project: resolved.slug,
      environment: options.environment,
      wait: options.wait,
      yes: options.yes,
    })
  } else {
    // Git-based deploy
    let branch = options.branch
    if (!branch) {
      const detectedBranch = detectGitBranch()
      if (detectedBranch && detectedBranch !== 'HEAD') {
        branch = detectedBranch
      }
    }

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

    // Show deployment preview
    newline()
    box(
      [
        `Project:     ${colors.bold(project.name)}`,
        `Environment: ${colors.bold(environmentName)}`,
        `Branch:      ${colors.bold(branch)}`,
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

    // Snapshot the pre-trigger latest id so we don't latch onto a prior deploy.
    // trigger-pipeline only enqueues a job; the new row appears asynchronously.
    const { data: baselineList } = await getProjectDeployments({
      client,
      path: { id: project.id },
      query: {
        per_page: 1,
        ...(environmentId ? { environment_id: environmentId } : {}),
      },
    })
    const afterId = baselineList?.deployments?.[0]?.id ?? 0

    // Trigger the pipeline
    startSpinner('Starting deployment...')

    try {
      const { data: pipelineData, error: pipelineError } = await triggerProjectPipeline({
        client,
        path: { id: project.id },
        body: {
          branch,
          environment_id: environmentId,
        },
      })

      if (pipelineError || !pipelineData) {
        failSpinner('Failed to start deployment')
        const msg = getErrorMessage(pipelineError)
        if (msg) {
          info(`Reason: ${msg}`)
        }
        return
      }

      succeedSpinner('Deployment started')
      info(pipelineData.message ?? 'Pipeline triggered successfully')

      if (options.wait === false) {
        newline()
        info('Deployment running in background')
        info(`Check status: ${colors.muted('bunx @temps-sdk/cli status')}`)
        return
      }

      // Wait for the deployment created by this trigger (not a prior completed one)
      startSpinner('Waiting for deployment to start...')
      const waitResult = await waitForTriggeredDeployment(
        async () => {
          const { data: deployList, error: deployError } = await getProjectDeployments({
            client,
            path: { id: project.id },
            query: {
              per_page: 10,
              ...(environmentId ? { environment_id: environmentId } : {}),
            },
          })
          return { deployments: deployList?.deployments, error: deployError }
        },
        { afterId },
      )

      if (!waitResult.ok && waitResult.error === 'fetch_failed') {
        failSpinner('Failed to fetch deployment status')
        info(`Error: ${getErrorMessage(waitResult.cause)}`)
        return
      }

      if (!waitResult.ok) {
        failSpinner('Could not locate the deployment to track')
        info(`Check status: ${colors.muted('bunx @temps-sdk/cli status')}`)
        return
      }

      succeedSpinner(`Deployment #${waitResult.deploymentId} found`)
      const result = await watchDeployment({
        projectId: project.id,
        deploymentId: waitResult.deploymentId,
        timeoutSecs: 600,
        projectName: resolved.slug,
      })

      if (!result.success) {
        process.exitCode = 1
      }
    } catch (err) {
      failSpinner('Deployment failed')
      throw err
    }
  }

  // Offer to save config if it doesn't exist
  if (!hasProjectConfig() && !options.yes) {
    newline()
    const save = await promptConfirm({
      message: 'Save this project link for future use?',
      default: true,
    })
    if (save) {
      await writeProjectConfig({ projectSlug: resolved.slug })
      info('Saved to .temps/config.json')
    }
  }
}

export function registerUpCommand(program: Command): void {
  program
    .command('up [project]')
    .description('Deploy the current project (runs setup wizard if not linked)')
    .option('-p, --project <project>', 'Project slug or ID')
    .option('-e, --environment <env>', 'Target environment name')
    .option('-b, --branch <branch>', 'Git branch to deploy (auto-detected from cwd)')
    .option('-n, --name <name>', 'Project name (for new projects)')
    .option('--preset <preset>', 'Framework preset slug (skip auto-detection)')
    .option('--manual', 'Use manual deployment mode (no git)')
    .option('--static', 'Deploy a pre-built static folder (no Docker, no git)')
    .option('--static-dir <dir>', 'Folder to upload for static deploys (auto-detected by default)')
    .option('--no-services', 'Skip external service setup')
    .option('--no-wait', 'Do not wait for deployment to complete')
    .option('-y, --yes', 'Skip confirmation prompts')
    .action((projectArg, opts) => {
      if (projectArg && !opts.project) {
        opts.project = projectArg
      }
      return up(projectArg, opts)
    })
}
