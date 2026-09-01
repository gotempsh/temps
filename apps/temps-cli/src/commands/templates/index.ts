// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { listPresets } from '../../api/sdk.gen.js'
import type { PresetResponse } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, type TableColumn } from '../../ui/table.js'
import { newline, header, icons, json, colors, info } from '../../ui/output.js'

export function registerTemplatesCommands(program: Command): void {
  const cmd = program
    .command('templates')
    .alias('tpl')
    .description('Browse deployment templates')

  cmd
    .command('list')
    .alias('ls')
    .description('List available templates')
    .option('--json', 'Output in JSON format')
    .option('--type <type>', 'Filter by project type (server, static)')
    .action(listTemplatesAction)
}

async function listTemplatesAction(options: { json?: boolean; type?: string }): Promise<void> {
  await requireAuth()
  await setupClient()

  const presetsData = await withSpinner('Fetching templates...', async () => {
    const { data, error } = await listPresets({ client })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  const presets = filterPresetsByType(presetsData?.presets ?? [], options.type)

  if (options.json) {
    json(presets)
    return
  }

  newline()
  header(`${icons.package} Available Templates (${presets.length})`)

  if (presets.length === 0) {
    info('No templates found')
    newline()
    return
  }

  const columns: TableColumn<PresetResponse>[] = [
    { header: 'Slug', key: 'slug', color: (v) => colors.bold(v) },
    { header: 'Label', key: 'label' },
    { header: 'Type', key: 'project_type', color: (v) => colors.primary(v) },
    {
      header: 'Port',
      accessor: (p) => formatPresetPort(p.default_port),
      color: (v) => (v === '-' ? colors.muted(v) : v),
    },
    { header: 'Description', key: 'description', color: (v) => colors.muted(v) },
  ]

  printTable(presets, columns, { style: 'minimal' })
  newline()
}

/**
 * --type is compared case-insensitively so `--type Server` matches presets
 * whose project_type is stored as "server" — otherwise a script would get a
 * silent empty result instead of the templates it expected.
 */
export function filterPresetsByType(
  presets: PresetResponse[],
  type: string | undefined
): PresetResponse[] {
  if (!type) return presets
  return presets.filter((p) => p.project_type.toLowerCase() === type.toLowerCase())
}

export function formatPresetPort(port: number | null | undefined): string {
  return port != null ? String(port) : '-'
}
