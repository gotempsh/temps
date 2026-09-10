// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { getProjectBySlug, getProjects } from '../../api/sdk.gen.js'
import { writeProjectConfig, hasProjectConfig, readProjectConfig } from '../../config/project-config.js'
import { promptSearch, promptSelect, promptConfirm } from '../../ui/prompts.js'
import { withSpinner } from '../../ui/spinner.js'
import { info, warning, newline, icons, colors, box } from '../../ui/output.js'

interface InitOptions {
  name?: string
  yes?: boolean
}

async function init(projectSlug: string | undefined, options: InitOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  newline()

  // Check if already initialized
  if (hasProjectConfig()) {
    const existing = await readProjectConfig()
    if (existing) {
      warning(`This directory is already linked to ${colors.bold(existing.projectSlug)}`)
      if (!options.yes) {
        const overwrite = await promptConfirm({
          message: 'Reinitialize?',
          default: false,
        })
        if (!overwrite) {
          info('Cancelled')
          return
        }
      }
    }
  }

  let slug: string

  if (projectSlug) {
    // Verify the provided slug
    const project = await withSpinner('Verifying project...', async () => {
      const { data, error } = await getProjectBySlug({
        client,
        path: { slug: projectSlug },
      })
      if (error || !data) {
        throw new Error(`Project "${projectSlug}" not found`)
      }
      return data
    })
    slug = projectSlug
    info(`Found project: ${colors.bold(project.name)}`)
  } else if (options.yes) {
    throw new Error('Project slug is required with --yes flag. Use: temps init <project-slug> --yes')
  } else {
    // Interactive: link existing or create new
    const action = await promptSelect({
      message: 'What would you like to do?',
      choices: [
        {
          name: 'Link existing project',
          value: 'link',
          description: 'Connect this directory to an existing Temps project',
        },
        {
          name: 'Create new project',
          value: 'create',
          description: 'Create a new project and link it here',
        },
      ],
    })

    if (action === 'create') {
      info('Starting project creation wizard...')
      info(`Run: ${colors.bold('temps projects create')}`)
      info('Then link with: temps link <project-slug>')
      return
    }

    // Link existing project
    const projects = await withSpinner('Fetching projects...', async () => {
      const { data, error } = await getProjects({
        client,
        query: { per_page: 100 },
      })
      if (error || !data) {
        throw new Error(getErrorMessage(error))
      }
      return data.projects ?? []
    })

    if (projects.length === 0) {
      warning('No projects found')
      info('Create one with: temps projects create')
      return
    }

    slug = await promptSearch({
      message: 'Select a project',
      choices: projects.map(p => ({
        name: `${p.name} (${p.slug})`,
        value: p.slug,
        description: p.main_branch ? `Branch: ${p.main_branch}` : undefined,
      })),
    })
  }

  // Write config
  const configPath = await writeProjectConfig({
    projectSlug: slug,
  })

  newline()
  box(
    [
      `Project: ${colors.bold(slug)}`,
      `Config: ${colors.muted(configPath)}`,
    ].join('\n'),
    `${icons.sparkles} Project Initialized`
  )

  newline()
  info('Next steps:')
  info(`  ${colors.muted('temps up')}        ${colors.muted('# Deploy this project')}`)
  info(`  ${colors.muted('temps status')}    ${colors.muted('# View project status')}`)
  info(`  ${colors.muted('temps env:pull')}  ${colors.muted('# Pull environment variables')}`)
}

export function registerInitCommand(program: Command): void {
  program
    .command('init [project-slug]')
    .description('Initialize a Temps project in the current directory')
    .option('-n, --name <name>', 'Project name (for new projects)')
    .option('-y, --yes', 'Skip confirmation prompts')
    .action(init)
}
