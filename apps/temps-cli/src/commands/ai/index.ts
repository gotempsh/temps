// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { requireProjectSlug } from '../../config/resolve-project.js'
import { getProjectBySlug, getChatReadiness } from '../../api/sdk.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import {
  newline,
  header,
  icons,
  colors,
  keyValue,
  json as jsonOutput,
} from '../../ui/output.js'

interface ReadinessOptions {
  project?: string
  json?: boolean
}

/**
 * Report which AI prerequisites a project satisfies, and — crucially — what to
 * do about the ones it doesn't. Scripting against the AI chat endpoints
 * otherwise means discovering the gates by getting a 409 back mid-run.
 */
async function readiness(
  projectArg: string | undefined,
  options: ReadinessOptions
): Promise<void> {
  await requireAuth()
  await setupClient()

  const resolved = await requireProjectSlug(projectArg ?? options.project)

  const project = await withSpinner('Resolving project...', async () => {
    const { data, error } = await getProjectBySlug({
      client,
      path: { slug: resolved.slug },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    if (!data) {
      throw new Error(`Project "${resolved.slug}" not found`)
    }
    return data
  })

  const status = await withSpinner('Checking AI readiness...', async () => {
    const { data, error } = await getChatReadiness({
      client,
      path: { project_id: project.id },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    if (!data) {
      throw new Error(`AI readiness returned no data for "${resolved.slug}"`)
    }
    return data
  })

  if (options.json) {
    jsonOutput(status)
    return
  }

  const mark = (ok: boolean) => (ok ? colors.success(icons.success) : colors.error(icons.error))

  newline()
  header(`AI readiness · ${project.name}`)
  newline()
  keyValue('AI provider configured', mark(status.ai_configured))
  keyValue('Chat enabled', mark(status.chat_enabled))
  keyValue('Write actions enabled', mark(status.write_actions_enabled))
  newline()

  // Each gate fails for a different reason and is fixed in a different place,
  // so name the specific next step rather than printing a generic "not ready".
  const fixes: string[] = []
  if (!status.ai_configured) {
    fixes.push(
      'Add an AI provider key in the dashboard under Settings → AI Providers (instance-wide).'
    )
  }
  if (!status.chat_enabled) {
    fixes.push(
      `Re-enable AI chat for this project: it has been explicitly turned off in ${resolved.slug}'s AI settings.`
    )
  }
  if (!status.write_actions_enabled) {
    fixes.push(
      'Enable AI write actions for this project (Settings → Security) to let the assistant propose changes such as alert rules. Every proposal still waits for your confirmation.'
    )
  }

  if (fixes.length === 0) {
    console.log(colors.success('Ready — the assistant can run and propose changes.'))
  } else {
    console.log(colors.warning('Not fully ready:'))
    for (const fix of fixes) {
      console.log(`  ${icons.bullet} ${fix}`)
    }
  }
  newline()
}

export function registerAiCommands(program: Command): void {
  const ai = program.command('ai').description('AI assistant status for a project')

  ai.command('readiness [project]')
    .description('Show which AI prerequisites this project meets, and how to fix the rest')
    .option('-p, --project <project>', 'Project slug')
    .option('--json', 'Output in JSON format')
    .action((projectArg, opts) => {
      if (projectArg && !opts.project) {
        opts.project = projectArg
      }
      return readiness(projectArg, opts)
    })
}
