// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  listFunnels,
  createFunnel,
  updateFunnel,
  deleteFunnel,
  getFunnelMetrics,
  previewFunnelMetrics,
} from '../../api/sdk.gen.js'
import type { FunnelResponse, StepConversionResponse, CreateFunnelStep } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, type TableColumn } from '../../ui/table.js'
import { promptText, promptConfirm } from '../../ui/prompts.js'
import { newline, header, icons, json, colors, success, info, warning, keyValue } from '../../ui/output.js'

interface ListOptions {
  projectId: string
  json?: boolean
}

interface CreateOptions {
  projectId: string
  name?: string
  steps?: string
  yes?: boolean
}

interface UpdateOptions {
  projectId: string
  funnelId: string
  name?: string
  steps?: string
}

interface RemoveOptions {
  projectId: string
  funnelId: string
  force?: boolean
  yes?: boolean
}

interface MetricsOptions {
  projectId: string
  funnelId: string
  json?: boolean
}

interface PreviewOptions {
  projectId: string
  steps: string
  json?: boolean
}

export function registerFunnelsCommands(program: Command): void {
  const funnels = program
    .command('funnels')
    .alias('funnel')
    .description('Manage analytics funnels for projects')

  funnels
    .command('list')
    .alias('ls')
    .description('List all funnels for a project')
    .requiredOption('--project-id <id>', 'Project ID')
    .option('--json', 'Output in JSON format')
    .action(listFunnelsAction)

  funnels
    .command('create')
    .alias('add')
    .description('Create a new funnel for a project')
    .requiredOption('--project-id <id>', 'Project ID')
    .option('-n, --name <name>', 'Funnel name')
    .option('-s, --steps <json>', 'Funnel steps as JSON array (e.g. \'[{"event_name":"page_view"},{"event_name":"signup"}]\')')
    .option('-y, --yes', 'Skip confirmation prompts (for automation)')
    .action(createFunnelAction)

  funnels
    .command('update')
    .description('Update a funnel')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--funnel-id <id>', 'Funnel ID')
    .option('-n, --name <name>', 'New funnel name')
    .option('-s, --steps <json>', 'New funnel steps as JSON array')
    .action(updateFunnelAction)

  funnels
    .command('remove')
    .alias('rm')
    .description('Delete a funnel')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--funnel-id <id>', 'Funnel ID')
    .option('-f, --force', 'Skip confirmation')
    .option('-y, --yes', 'Skip confirmation prompts (alias for --force)')
    .action(removeFunnelAction)

  funnels
    .command('metrics')
    .description('Get funnel metrics')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--funnel-id <id>', 'Funnel ID')
    .option('--json', 'Output in JSON format')
    .action(getFunnelMetricsAction)

  funnels
    .command('preview')
    .description('Preview funnel metrics without saving')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('-s, --steps <json>', 'Funnel steps as JSON array')
    .option('--json', 'Output in JSON format')
    .action(previewFunnelMetricsAction)
}

export function parseStepsJson(stepsStr: string): CreateFunnelStep[] | null {
  try {
    const parsed = JSON.parse(stepsStr)
    if (!Array.isArray(parsed)) {
      warning('Steps must be a JSON array')
      return null
    }
    for (const step of parsed) {
      if (!step.event_name || typeof step.event_name !== 'string') {
        warning('Each step must have an "event_name" string field')
        return null
      }
    }
    return parsed as CreateFunnelStep[]
  } catch {
    warning('Invalid JSON for steps. Expected a JSON array of step definitions.')
    info('Example: \'[{"event_name":"page_view"},{"event_name":"signup"}]\'')
    return null
  }
}

async function listFunnelsAction(options: ListOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = parseInt(options.projectId, 10)
  if (isNaN(projectId)) {
    warning('Invalid project ID')
    return
  }

  const funnelsData = await withSpinner('Fetching funnels...', async () => {
    const { data, error } = await listFunnels({
      client,
      path: { project_id: projectId },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data ?? []
  })

  if (options.json) {
    json(funnelsData)
    return
  }

  newline()
  header(`${icons.info} Funnels for Project ${projectId} (${funnelsData.length})`)

  if (funnelsData.length === 0) {
    info('No funnels configured')
    info(`Run: temps funnels create --project-id ${projectId} --name "My Funnel" --steps '[{"event_name":"page_view"},{"event_name":"signup"}]' -y`)
    newline()
    return
  }

  const columns: TableColumn<FunnelResponse>[] = [
    { header: 'ID', key: 'id', width: 6 },
    { header: 'Name', key: 'name', color: (v) => colors.bold(v) },
    { header: 'Active', accessor: (f) => f.is_active ? 'yes' : 'no', color: (v) => v === 'yes' ? colors.success(v) : colors.muted(v) },
    { header: 'Created', accessor: (f) => new Date(f.created_at).toLocaleString() },
    { header: 'Updated', accessor: (f) => new Date(f.updated_at).toLocaleString() },
  ]

  printTable(funnelsData, columns, { style: 'minimal' })
  newline()
}

async function createFunnelAction(options: CreateOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = parseInt(options.projectId, 10)
  if (isNaN(projectId)) {
    warning('Invalid project ID')
    return
  }

  let name: string
  let steps: CreateFunnelStep[]

  const isAutomation = options.yes && options.name && options.steps

  if (isAutomation) {
    name = options.name!
    const parsed = parseStepsJson(options.steps!)
    if (!parsed) return
    steps = parsed
  } else {
    name = options.name || await promptText({
      message: 'Funnel name',
      required: true,
    })

    const stepsInput = options.steps || await promptText({
      message: 'Funnel steps (JSON array)',
      required: true,
    })

    const parsed = parseStepsJson(stepsInput)
    if (!parsed) return
    steps = parsed
  }

  const result = await withSpinner('Creating funnel...', async () => {
    const { data, error } = await createFunnel({
      client,
      path: { project_id: projectId },
      body: {
        name,
        steps,
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  success(`Funnel "${name}" created successfully`)
  if (result?.funnel_id) {
    info(`Funnel ID: ${result.funnel_id}`)
  }
  info(`Steps: ${steps.length} step(s)`)
}

async function updateFunnelAction(options: UpdateOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = parseInt(options.projectId, 10)
  const funnelId = parseInt(options.funnelId, 10)
  if (isNaN(projectId) || isNaN(funnelId)) {
    warning('Invalid project or funnel ID')
    return
  }

  if (!options.name && !options.steps) {
    warning('No fields to update. Provide at least one of --name or --steps')
    return
  }

  // Build the update body - the API requires the full CreateFunnelRequest shape
  // First, fetch current funnel data to fill in missing fields
  const currentFunnels = await withSpinner('Fetching current funnel...', async () => {
    const { data, error } = await listFunnels({
      client,
      path: { project_id: projectId },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data ?? []
  })

  const currentFunnel = currentFunnels.find((f) => f.id === funnelId)
  if (!currentFunnel) {
    warning(`Funnel ${funnelId} not found in project ${projectId}`)
    return
  }

  let steps: CreateFunnelStep[] | undefined
  if (options.steps) {
    const parsed = parseStepsJson(options.steps)
    if (!parsed) return
    steps = parsed
  }

  const body = {
    name: options.name ?? currentFunnel.name,
    steps: steps ?? ([] as CreateFunnelStep[]),
  }

  await withSpinner('Updating funnel...', async () => {
    const { error } = await updateFunnel({
      client,
      path: { project_id: projectId, funnel_id: funnelId },
      body,
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success(`Funnel #${funnelId} updated`)

  if (options.name) {
    info(`Name: ${options.name}`)
  }
  if (steps) {
    info(`Steps: ${steps.length} step(s)`)
  }
}

async function removeFunnelAction(options: RemoveOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = parseInt(options.projectId, 10)
  const funnelId = parseInt(options.funnelId, 10)
  if (isNaN(projectId) || isNaN(funnelId)) {
    warning('Invalid project or funnel ID')
    return
  }

  const skipConfirmation = options.force || options.yes

  if (!skipConfirmation) {
    const confirmed = await promptConfirm({
      message: `Delete funnel #${funnelId}?`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  await withSpinner('Deleting funnel...', async () => {
    const { error } = await deleteFunnel({
      client,
      path: { project_id: projectId, funnel_id: funnelId },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success('Funnel deleted')
}

async function getFunnelMetricsAction(options: MetricsOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = parseInt(options.projectId, 10)
  const funnelId = parseInt(options.funnelId, 10)
  if (isNaN(projectId) || isNaN(funnelId)) {
    warning('Invalid project or funnel ID')
    return
  }

  const metrics = await withSpinner('Fetching funnel metrics...', async () => {
    const { data, error } = await getFunnelMetrics({
      client,
      path: { project_id: projectId, funnel_id: funnelId },
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error) ?? 'Failed to get funnel metrics')
    }
    return data
  })

  if (options.json) {
    json(metrics)
    return
  }

  newline()
  header(`${icons.info} Funnel Metrics: ${metrics.funnel_name}`)
  keyValue('Funnel ID', metrics.funnel_id)
  keyValue('Total Entries', metrics.total_entries)
  keyValue('Overall Conversion Rate', `${metrics.overall_conversion_rate.toFixed(2)}%`)
  keyValue('Avg Completion Time', `${metrics.average_completion_time_seconds.toFixed(1)}s`)

  if (metrics.step_conversions && metrics.step_conversions.length > 0) {
    newline()
    header('Step Conversions')

    const columns: TableColumn<StepConversionResponse>[] = [
      { header: '#', accessor: (s) => s.step_order.toString(), width: 4 },
      { header: 'Step', key: 'step_name', color: (v) => colors.bold(v) },
      { header: 'Completions', accessor: (s) => s.completions.toString() },
      { header: 'Conversion', accessor: (s) => `${s.conversion_rate.toFixed(2)}%`, color: (v) => {
        const rate = parseFloat(v)
        return rate >= 50 ? colors.success(v) : rate >= 20 ? colors.warning(v) : colors.error(v)
      }},
      { header: 'Drop-off', accessor: (s) => `${s.drop_off_rate.toFixed(2)}%` },
      { header: 'Avg Time', accessor: (s) => `${s.average_time_to_complete_seconds.toFixed(1)}s` },
    ]

    printTable(metrics.step_conversions, columns, { style: 'minimal' })
  }

  newline()
}

async function previewFunnelMetricsAction(options: PreviewOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = parseInt(options.projectId, 10)
  if (isNaN(projectId)) {
    warning('Invalid project ID')
    return
  }

  const steps = parseStepsJson(options.steps)
  if (!steps) return

  const metrics = await withSpinner('Previewing funnel metrics...', async () => {
    const { data, error } = await previewFunnelMetrics({
      client,
      path: { project_id: projectId },
      body: {
        name: 'Preview',
        steps,
      },
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error) ?? 'Failed to preview funnel metrics')
    }
    return data
  })

  if (options.json) {
    json(metrics)
    return
  }

  newline()
  header(`${icons.info} Funnel Preview`)
  keyValue('Total Entries', metrics.total_entries)
  keyValue('Overall Conversion Rate', `${metrics.overall_conversion_rate.toFixed(2)}%`)
  keyValue('Avg Completion Time', `${metrics.average_completion_time_seconds.toFixed(1)}s`)

  if (metrics.step_conversions && metrics.step_conversions.length > 0) {
    newline()
    header('Step Conversions')

    const columns: TableColumn<StepConversionResponse>[] = [
      { header: '#', accessor: (s) => s.step_order.toString(), width: 4 },
      { header: 'Step', key: 'step_name', color: (v) => colors.bold(v) },
      { header: 'Completions', accessor: (s) => s.completions.toString() },
      { header: 'Conversion', accessor: (s) => `${s.conversion_rate.toFixed(2)}%`, color: (v) => {
        const rate = parseFloat(v)
        return rate >= 50 ? colors.success(v) : rate >= 20 ? colors.warning(v) : colors.error(v)
      }},
      { header: 'Drop-off', accessor: (s) => `${s.drop_off_rate.toFixed(2)}%` },
      { header: 'Avg Time', accessor: (s) => `${s.average_time_to_complete_seconds.toFixed(1)}s` },
    ]

    printTable(metrics.step_conversions, columns, { style: 'minimal' })
  }

  newline()
}
