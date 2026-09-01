// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { getProjectBySlug, getProjects, getEnvironments } from '../../api/sdk.gen.js'
import { writeProjectConfig, hasProjectConfig, readProjectConfig } from '../../config/project-config.js'
import { promptSearch, promptSelect, promptConfirm } from '../../ui/prompts.js'
import { withSpinner } from '../../ui/spinner.js'
import { info, warning, newline, icons, colors, box } from '../../ui/output.js'

interface LinkOptions {
  environment?: string
}

interface SearchChoice {
  name: string
  value: string
  description?: string
}

/** Shape the project search prompt's choices: label, slug, and an optional branch hint. */
export function toProjectChoices(
  projects: Array<{ name: string; slug: string; main_branch?: string | null }>,
): SearchChoice[] {
  return projects.map((p) => ({
    name: `${p.name} (${p.slug})`,
    value: p.slug,
    description: p.main_branch ? `Branch: ${p.main_branch}` : undefined,
  }))
}

/** Shape the environment select prompt's choices, flagging previews. */
export function toEnvironmentChoices(
  envs: Array<{ name: string; is_preview?: boolean }>,
): SearchChoice[] {
  return envs.map((e) => ({
    name: e.name,
    value: e.name,
    description: e.is_preview ? 'Preview' : undefined,
  }))
}

async function link(projectSlug: string | undefined, options: LinkOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  newline()

  // Warn if already linked
  if (hasProjectConfig()) {
    const existing = await readProjectConfig()
    if (existing) {
      warning(`This directory is already linked to ${colors.bold(existing.projectSlug)}`)
      const overwrite = await promptConfirm({
        message: 'Overwrite existing link?',
        default: false,
      })
      if (!overwrite) {
        info('Cancelled')
        return
      }
    }
  }

  let slug: string

  if (projectSlug) {
    // Verify the project exists
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
  } else {
    // Fetch projects and let user search
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
      warning('No projects found. Create one first with: temps projects create')
      return
    }

    slug = await promptSearch({
      message: 'Select a project to link',
      choices: toProjectChoices(projects),
    })
  }

  // Optionally select a default environment
  let environmentName = options.environment
  if (!environmentName) {
    try {
      const { data: projectData } = await getProjectBySlug({
        client,
        path: { slug: slug },
      })
      if (projectData) {
        const { data: envs } = await getEnvironments({
          client,
          path: { project_id: projectData.id },
        })
        if (envs && envs.length > 1) {
          const setDefault = await promptConfirm({
            message: 'Set a default environment?',
            default: false,
          })
          if (setDefault) {
            environmentName = await promptSelect({
              message: 'Default environment',
              choices: toEnvironmentChoices(envs),
            })
          }
        }
      }
    } catch {
      // Non-critical, skip environment selection
    }
  }

  // Write config
  const configPath = await writeProjectConfig({
    projectSlug: slug,
    environmentName,
  })

  newline()
  box(
    [
      `Project: ${colors.bold(slug)}`,
      environmentName ? `Environment: ${colors.bold(environmentName)}` : null,
      `Config: ${colors.muted(configPath)}`,
    ]
      .filter(Boolean)
      .join('\n'),
    `${icons.check} Directory Linked`
  )

  newline()
  info('You can now run commands without specifying --project:')
  info(`  ${colors.muted('temps up')}        ${colors.muted('# Deploy this project')}`)
  info(`  ${colors.muted('temps status')}    ${colors.muted('# View project status')}`)
  info(`  ${colors.muted('temps env:pull')}  ${colors.muted('# Pull environment variables')}`)
}

export function registerLinkCommand(program: Command): void {
  program
    .command('link [project-slug]')
    .description('Link current directory to a Temps project')
    .option('-e, --environment <name>', 'Set default environment')
    .action(link)
}
