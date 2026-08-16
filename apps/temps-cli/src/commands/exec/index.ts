import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { requireProjectSlug } from '../../config/resolve-project.js'
import { getProjectBySlug, getEnvironments, listContainers } from '../../api/sdk.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import {
  info,
  warning,
  newline,
  colors,
  header,
  icons,
} from '../../ui/output.js'

interface ExecOptions {
  project?: string
  environment?: string
}

/**
 * Pick which environment's containers to show as "helpful context".
 *
 * Matches by name (case-insensitive) or exact slug so `--environment prod`
 * and `--environment Production` both work; falls back to the first
 * environment when no `--environment` flag was given.
 */
export function selectTargetEnvironment<T extends { name: string; slug: string }>(
  environments: T[],
  environmentOption?: string,
): T | undefined {
  if (!environmentOption) return environments[0]
  return environments.find(
    (e) => e.name.toLowerCase() === environmentOption.toLowerCase() || e.slug === environmentOption
  )
}

async function exec(_command: string | undefined, options: ExecOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const resolved = await requireProjectSlug(options.project)

  if (resolved.source !== 'flag') {
    newline()
    info(`Using project ${colors.bold(resolved.slug)} (from ${resolved.source})`)
  }

  newline()
  header(`${icons.warning} Remote Execution Not Yet Available`)
  newline()
  info('Remote command execution requires a server-side agent that is not yet implemented.')
  newline()
  info(`${colors.bold('Alternatives:')}`)
  info(`  ${colors.muted('1.')} SSH into your server directly`)
  info(`  ${colors.muted('2.')} Use the dashboard to view container details`)
  info(`  ${colors.muted('3.')} Check container logs: ${colors.bold('temps containers logs <id>')}`)

  // Show available containers as helpful context
  try {
    const containers = await withSpinner('Fetching containers...', async () => {
      const { data: project, error: projectError } = await getProjectBySlug({
        client,
        path: { slug: resolved.slug },
      })
      if (projectError || !project) {
        throw new Error(`Project "${resolved.slug}" not found`)
      }

      const { data: environments, error: envsError } = await getEnvironments({
        client,
        path: { project_id: project.id },
      })
      if (envsError || !environments || environments.length === 0) return []

      // Fetch containers from the first environment (or specified one)
      const targetEnv = selectTargetEnvironment(environments, options.environment)

      if (!targetEnv) return []

      const { data, error } = await listContainers({
        client,
        path: { project_id: project.id, environment_id: targetEnv.id },
      })
      if (error) throw new Error(getErrorMessage(error))
      return data?.containers ?? []
    })

    if (containers.length > 0) {
      newline()
      info(`${colors.bold('Running containers:')}`)
      for (const container of containers) {
        const containerStatus = container.status === 'running'
          ? colors.success('running')
          : colors.warning(container.status)
        info(`  ${colors.muted('●')} ${container.container_name ?? container.container_id} (${containerStatus})`)
      }
    }
  } catch {
    // Silently ignore if we can't fetch containers
  }

  newline()
  warning('This feature will be available in a future release.')
}

export function registerExecCommands(program: Command): void {
  program
    .command('exec [command]')
    .alias('ssh')
    .description('Execute a command in a running container (coming soon)')
    .option('-p, --project <project>', 'Project slug')
    .option('-e, --environment <env>', 'Target environment')
    .action(exec)
}
