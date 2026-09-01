// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { requireProjectSlug } from '../../config/resolve-project.js'
import { rollback as rollbackDeploy } from '../deploy/rollback.js'
import { info, colors, newline } from '../../ui/output.js'

interface RollbackOptions {
  project?: string
  environment?: string
  to?: string
  yes?: boolean
}

async function rollback(projectArg: string | undefined, options: RollbackOptions): Promise<void> {
  const resolved = await requireProjectSlug(projectArg ?? options.project)

  if (resolved.source !== 'flag') {
    newline()
    info(`Using project ${colors.bold(resolved.slug)} (from ${resolved.source})`)
  }

  await rollbackDeploy({
    project: resolved.slug,
    environment: options.environment ?? 'production',
    to: options.to,
  })
}

export function registerRollbackCommand(program: Command): void {
  program
    .command('rollback [project]')
    .description('Rollback to a previous deployment')
    .option('-p, --project <project>', 'Project slug')
    .option('-e, --environment <env>', 'Target environment', 'production')
    .option('--to <id>', 'Rollback to specific deployment ID')
    .option('-y, --yes', 'Skip confirmation')
    .action((projectArg, opts) => {
      if (projectArg && !opts.project) {
        opts.project = projectArg
      }
      return rollback(projectArg, opts)
    })
}
