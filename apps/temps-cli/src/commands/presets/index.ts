// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { listPresets } from '../../api/sdk.gen.js'
import type { PresetResponse } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, type TableColumn } from '../../ui/table.js'
import { newline, header, icons, json, colors, info, keyValue } from '../../ui/output.js'

export function registerPresetsCommands(program: Command): void {
  const presets = program
    .command('presets')
    .alias('preset')
    .description('Browse available build presets')

  presets
    .command('list')
    .alias('ls')
    .description('List available presets')
    .option('--json', 'Output in JSON format')
    .option('--type <type>', 'Filter by project type (server, static)')
    .action(listPresetsAction)

  presets
    .command('show <slug>')
    .alias('get')
    .description('Show details for a specific preset')
    .option('--json', 'Output in JSON format')
    .action(showPresetAction)
}

/**
 * Case-insensitive filter on `project_type` — a preset's type is compared
 * lowercased on both sides so `--type Server` and `--type server` behave
 * identically instead of one silently returning an empty list.
 */
export function filterPresetsByType(presets: PresetResponse[], type?: string): PresetResponse[] {
  if (!type) return presets
  return presets.filter((p) => p.project_type.toLowerCase() === type.toLowerCase())
}

async function listPresetsAction(options: { json?: boolean; type?: string }): Promise<void> {
  await requireAuth()
  await setupClient()

  const presetsData = await withSpinner('Fetching presets...', async () => {
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
  header(`${icons.package} Available Presets (${presets.length})`)

  if (presets.length === 0) {
    info('No presets found')
    newline()
    return
  }

  const columns: TableColumn<PresetResponse>[] = [
    { header: 'Slug', key: 'slug', color: (v) => colors.bold(v) },
    { header: 'Label', key: 'label' },
    { header: 'Type', key: 'project_type', color: (v) => colors.primary(v) },
    {
      header: 'Port',
      accessor: (p) => (p.default_port != null ? String(p.default_port) : '-'),
      color: (v) => (v === '-' ? colors.muted(v) : v),
    },
    { header: 'Description', key: 'description', color: (v) => colors.muted(v) },
  ]

  printTable(presets, columns, { style: 'minimal' })
  newline()
}

async function showPresetAction(slug: string, options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const presetsData = await withSpinner('Fetching preset...', async () => {
    const { data, error } = await listPresets({ client })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  const presets = presetsData?.presets ?? []
  const preset = presets.find((p) => p.slug === slug)

  if (!preset) {
    info(`Preset "${slug}" not found`)
    newline()
    info('Available presets:')
    for (const p of presets) {
      info(`  ${colors.bold(p.slug)} - ${p.label}`)
    }
    newline()
    return
  }

  if (options.json) {
    json(preset)
    return
  }

  newline()
  header(`${icons.package} ${preset.label}`)
  keyValue('Slug', preset.slug)
  keyValue('Label', preset.label)
  keyValue('Description', preset.description)
  keyValue('Project Type', preset.project_type)
  keyValue('Default Port', preset.default_port != null ? String(preset.default_port) : colors.muted('none (static)'))
  keyValue('Icon', preset.icon_url || colors.muted('not set'))
  newline()
}
