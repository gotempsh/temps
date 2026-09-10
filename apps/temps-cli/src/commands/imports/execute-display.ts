// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Renders an `ExecuteImportResponse` — the real per-step outcome of running
 * an import (create services, populate data, deploy, verify the app
 * actually responds). Each step is shown with a clear pass/fail/skip icon
 * and its honest message, since a step can succeed, fail, or be
 * intentionally skipped (e.g. "source database not reachable").
 */

import chalk from 'chalk'
import { header, newline, keyValue, colors, icons, success, error as printError } from '../../ui/output.js'
import type { ExecuteImportResponse, StepResult } from '../../api/types.gen.js'
import { getWebUrl } from '../../lib/api-client.js'

export function stepIcon(step: StepResult): string {
  if (step.skipped) return chalk.gray('○')
  return step.success ? icons.check : icons.cross
}

export function displayExecuteResult(result: ExecuteImportResponse): void {
  newline()
  header(`${icons.rocket} Import Execution`)

  for (const step of result.step_results) {
    const label = step.skipped ? chalk.gray(step.step_title) : colors.bold(step.step_title)
    console.log(`  ${stepIcon(step)} ${label} ${colors.muted(`(${step.duration_seconds.toFixed(1)}s)`)}`)
    console.log(`    ${step.skipped ? chalk.gray(step.message) : step.success ? colors.muted(step.message) : chalk.red(step.message)}`)
    for (const resource of step.created_resources) {
      console.log(`    ${colors.muted(icons.bullet)} ${colors.muted(`${resource.resource_type} '${resource.resource_name}' (id ${resource.resource_id})`)}`)
    }
  }

  newline()
  if (result.status === 'completed') {
    success('Import completed — the app is live and verified as responding.')
  } else {
    printError(`Import finished with status: ${result.status}`)
    console.log(
      `  ${colors.muted('The project and any created resources are still there — check the failed step above and retry once resolved.')}`,
    )
  }

  if (result.project_id) {
    keyValue('Project ID', String(result.project_id))
    keyValue('Dashboard', `${getWebUrl()}/projects/${result.project_id}`)
  }
  newline()
}
