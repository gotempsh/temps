// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import {
  info,
  warning,
  newline,
  colors,
  header,
  icons,
} from '../../ui/output.js'

async function dev(): Promise<void> {
  newline()
  header(`${icons.warning} Dev Mode Coming Soon`)
  newline()
  info('The dev command will provide a local development tunnel,')
  info('allowing you to expose your local server to the internet')
  info('for testing webhooks, OAuth callbacks, and more.')
  newline()
  info(`${colors.bold('In the meantime, you can:')}`)
  info(`  ${colors.muted('1.')} Deploy preview environments: ${colors.bold('temps up -e preview')}`)
  info(`  ${colors.muted('2.')} Use environment variables locally: ${colors.bold('temps env:pull')}`)
  info(`  ${colors.muted('3.')} Check project status: ${colors.bold('temps status')}`)
  newline()
  warning('This feature will be available in a future release.')
}

export function registerDevCommand(program: Command): void {
  program
    .command('dev')
    .description('Start a local development tunnel (coming soon)')
    .option('-p, --project <project>', 'Project slug')
    .option('--port <port>', 'Local port to expose', '3000')
    .action(dev)
}
