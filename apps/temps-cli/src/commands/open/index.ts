// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { execSync } from 'node:child_process'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getWebUrl, getErrorMessage } from '../../lib/api-client.js'
import { requireProjectSlug } from '../../config/resolve-project.js'
import { getProjectBySlug, getEnvironments } from '../../api/sdk.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { promptSelect } from '../../ui/prompts.js'
import {
  success,
  info,
  newline,
  colors,
  error as errorOutput,
} from '../../ui/output.js'

interface OpenOptions {
  project?: string
  environment?: string
  dashboard?: boolean
}

function openUrl(url: string): void {
  const platform = process.platform
  try {
    if (platform === 'darwin') {
      execSync(`open "${url}"`)
    } else if (platform === 'win32') {
      execSync(`start "" "${url}"`)
    } else {
      execSync(`xdg-open "${url}"`)
    }
  } catch {
    // Fallback: just print the URL
    info(`Open in browser: ${colors.primary(url)}`)
  }
}

async function open(projectArg: string | undefined, options: OpenOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const resolved = await requireProjectSlug(projectArg ?? options.project)

  if (resolved.source !== 'flag') {
    newline()
    info(`Using project ${colors.bold(resolved.slug)} (from ${resolved.source})`)
  }

  // If --dashboard, open the web dashboard
  if (options.dashboard) {
    const webUrl = getWebUrl()
    const dashboardUrl = `${webUrl}/projects/${resolved.slug}`
    success(`Opening dashboard for ${resolved.slug}`)
    openUrl(dashboardUrl)
    return
  }

  // Fetch project and environments to get URLs
  const result = await withSpinner('Fetching project info...', async () => {
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
    if (envsError) {
      throw new Error(getErrorMessage(envsError))
    }

    return { project, environments: environments ?? [] }
  })

  if (result.environments.length === 0) {
    errorOutput('No environments found for this project')
    return
  }

  // Determine which environment to open
  let targetEnv: typeof result.environments[number] | undefined

  if (options.environment) {
    targetEnv = result.environments.find(
      e => e.name.toLowerCase() === options.environment!.toLowerCase() ||
           e.slug === options.environment
    )
    if (!targetEnv) {
      errorOutput(`Environment "${options.environment}" not found`)
      info(`Available: ${result.environments.map(e => e.name).join(', ')}`)
      return
    }
  } else if (result.environments.length > 1) {
    const envName = await promptSelect({
      message: 'Open which environment?',
      choices: result.environments.map(e => ({
        name: `${e.name} ${e.is_preview ? '(preview)' : ''} - ${e.main_url}`,
        value: e.name,
      })),
    })
    targetEnv = result.environments.find(e => e.name === envName)
  } else {
    targetEnv = result.environments[0]
  }

  if (!targetEnv) {
    errorOutput('Could not determine environment')
    return
  }

  if (!targetEnv.main_url) {
    errorOutput(`No URL configured for environment "${targetEnv.name}"`)
    info('Deploy the project first to get a URL')
    return
  }

  const url = targetEnv.main_url.startsWith('http')
    ? targetEnv.main_url
    : `https://${targetEnv.main_url}`

  success(`Opening ${targetEnv.name}: ${colors.primary(url)}`)
  openUrl(url)
}

export function registerOpenCommand(program: Command): void {
  program
    .command('open [project]')
    .description('Open project URL in browser')
    .option('-p, --project <project>', 'Project slug')
    .option('-e, --environment <env>', 'Open specific environment')
    .option('--dashboard', 'Open the dashboard instead of the project URL')
    .action((projectArg, opts) => {
      if (projectArg && !opts.project) {
        opts.project = projectArg
      }
      return open(projectArg, opts)
    })
}
