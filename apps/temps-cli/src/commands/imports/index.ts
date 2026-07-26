import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { discoverWorkloads, createPlan, executeImport, getImportStatus, listSources } from '../../api/sdk.gen.js'
import type { ImportSource, ImportSourceInfo, WorkloadDescriptor, ImportCredentials } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, statusBadge, type TableColumn } from '../../ui/table.js'
import { promptSelect, promptConfirm, promptText } from '../../ui/prompts.js'
import { newline, header, icons, json, colors, success, info, warning, keyValue, error as printError } from '../../ui/output.js'
import { resolveCredentials, describeCredentials, type CredentialFlags } from './credentials.js'
import { displayImportPlan } from './plan-display.js'
import { displayExecuteResult } from './execute-display.js'
import { runVercelMigrationWizard } from '../migrate/index.js'

const VERCEL_SOURCE: ImportSourceInfo = {
  source: 'vercel',
  name: 'Vercel',
  version: 'client-side',
  available: true,
  capabilities: {
    requires_credentials: true,
    supports_build: true,
    supports_cost_analysis: false,
    supports_domains: true,
    supports_health_checks: false,
    supports_networks: false,
    supports_project_snapshot: true,
    supports_resource_limits: false,
    supports_services: true,
    supports_volumes: false,
  },
}

interface SourcesOptions {
  json?: boolean
}

interface CommonSourceOptions extends CredentialFlags {
  source?: string
  json?: boolean
}

interface DiscoverOptions extends CommonSourceOptions {}

interface PlanOptions extends CommonSourceOptions {
  workload?: string
}

interface RunOptions extends CommonSourceOptions {
  workload?: string
  projectName?: string
  preset?: string
  directory?: string
  branch?: string
  dryRun?: boolean
  yes?: boolean
}

interface StatusOptions {
  sessionId: string
  json?: boolean
}

export function registerImportsCommands(program: Command): void {
  const imports = program
    .command('migrate')
    .alias('imports')
    .alias('import')
    .description('Migrate a project from another platform (Vercel, Coolify, Dokploy, CapRover, Portainer, Kubernetes, Docker) into temps')

  imports
    .command('sources')
    .alias('ls')
    .description('List available import sources')
    .option('--json', 'Output in JSON format')
    .action(listSourcesAction)

  imports
    .command('discover')
    .description('Discover workloads from a source')
    .option('-s, --source <source>', 'Import source (coolify, dokploy, caprover, portainer, kubernetes, kamal, docker)')
    .option('--token <token>', 'API token / admin password for the source instance')
    .option('--base-url <url>', 'Base URL of the source instance')
    .option('--username <name>', 'Admin username (portainer source, defaults to "admin")')
    .option('--kubeconfig <path>', 'Path to a kubeconfig file (kubernetes source)')
    .option('--deploy-yml <path>', 'Path to config/deploy.yml (kamal source)')
    .option('--json', 'Output in JSON format')
    .action(discoverAction)

  imports
    .command('plan')
    .description('Discover a source, pick a workload, and show the import plan')
    .option('-s, --source <source>', 'Import source')
    .option('-w, --workload <workload>', 'Workload ID to import (skips the picker)')
    .option('--token <token>', 'API token / admin password for the source instance')
    .option('--base-url <url>', 'Base URL of the source instance')
    .option('--username <name>', 'Admin username (portainer source, defaults to "admin")')
    .option('--kubeconfig <path>', 'Path to a kubeconfig file (kubernetes source)')
    .option('--deploy-yml <path>', 'Path to config/deploy.yml (kamal source)')
    .action(planAction)

  imports
    .command('run')
    .description('Guided end-to-end migration: discover, plan, review, and execute')
    .option('-s, --source <source>', 'Import source (vercel, coolify, dokploy, caprover, portainer, kubernetes, kamal, docker)')
    .option('-w, --workload <workload>', 'Workload ID to import (skips the picker)')
    .option('--token <token>', 'API token / admin password for the source instance')
    .option('--base-url <url>', 'Base URL of the source instance')
    .option('--username <name>', 'Admin username (portainer source, defaults to "admin")')
    .option('--kubeconfig <path>', 'Path to a kubeconfig file (kubernetes source)')
    .option('--deploy-yml <path>', 'Path to config/deploy.yml (kamal source)')
    .option('--project-name <name>', 'Name for the new temps project (defaults to the source project name)')
    .option('--preset <preset>', 'Build preset (defaults to "nixpacks" for git sources, "dockerfile" otherwise)')
    .option('--directory <dir>', 'Project subdirectory', '.')
    .option('--branch <branch>', 'Branch to deploy', 'main')
    .option('--dry-run', 'Plan only — do not create or deploy anything')
    .option('-y, --yes', 'Skip the confirmation prompt (for automation)')
    .action(runAction)

  imports
    .command('execute')
    .description('Execute a previously created import plan by session ID')
    .requiredOption('--session-id <id>', 'Import session ID (from `imports plan`)')
    .option('--project-name <name>', 'Name for the new temps project')
    .option('--preset <preset>', 'Build preset', 'nixpacks')
    .option('--directory <dir>', 'Project subdirectory', '.')
    .option('--branch <branch>', 'Branch to deploy', 'main')
    .option('--dry-run', 'Plan only — do not create or deploy anything')
    .option('-y, --yes', 'Skip the confirmation prompt (for automation)')
    .action(executeSessionAction)

  imports
    .command('status')
    .description('Show a stored import session (the plan it was created with)')
    .requiredOption('--session-id <id>', 'Import session ID')
    .option('--json', 'Output in JSON format')
    .action(statusAction)
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

async function fetchSources(): Promise<ImportSourceInfo[]> {
  const { data, error } = await listSources({ client })
  if (error) throw new Error(getErrorMessage(error))
  return data ?? []
}

async function pickSource(preselected: string | undefined): Promise<ImportSourceInfo> {
  const sources = await withSpinner('Fetching import sources...', fetchSources)
  if (sources.length === 0) throw new Error('No import sources available on this server')

  if (preselected) {
    const found = sources.find((s) => s.source === preselected)
    if (!found) {
      throw new Error(`Unknown source '${preselected}'. Available: ${sources.map((s) => s.source).join(', ')}`)
    }
    return found
  }

  const source = await promptSelect({
    message: 'Select import source',
    choices: sources.map((s) => ({
      name: `${s.name}${s.available ? '' : colors.muted(' (unavailable)')}`,
      value: s.source,
      disabled: !s.available,
    })),
  })
  return sources.find((s) => s.source === source) as ImportSourceInfo
}

async function pickSourceForMigration(preselected: string | undefined): Promise<ImportSourceInfo> {
  const sources = await withSpinner('Fetching import sources...', fetchSources)
  const allSources = [...sources, VERCEL_SOURCE]

  if (preselected) {
    const found = allSources.find((s) => s.source === preselected)
    if (!found) {
      throw new Error(`Unknown source '${preselected}'. Available: ${allSources.map((s) => s.source).join(', ')}`)
    }
    return found
  }

  const source = await promptSelect({
    message: 'Select source platform to migrate from',
    choices: allSources.map((s) => ({
      name: `${s.name}${s.available ? '' : colors.muted(' (unavailable)')}`,
      value: s.source,
      disabled: !s.available,
    })),
  })
  return allSources.find((s) => s.source === source) as ImportSourceInfo
}

async function discover(source: ImportSource, credentials: ImportCredentials): Promise<WorkloadDescriptor[]> {
  const { data, error } = await withSpinner(`Discovering workloads from ${source}...`, () =>
    discoverWorkloads({ client, body: { source, credentials } }),
  )
  if (error) throw new Error(getErrorMessage(error))
  return data?.workloads ?? []
}

async function pickWorkload(workloads: WorkloadDescriptor[], preselected: string | undefined): Promise<WorkloadDescriptor> {
  if (preselected) {
    const found = workloads.find((w) => w.id === preselected)
    if (!found) throw new Error(`Workload '${preselected}' not found in discovery results`)
    return found
  }

  const id = await promptSelect({
    message: 'Select workload to import',
    choices: workloads.map((w) => ({
      name: `${w.name ?? w.id} ${colors.muted(`(${w.workload_type}, ${w.status})`)}`,
      value: w.id,
    })),
  })
  return workloads.find((w) => w.id === id) as WorkloadDescriptor
}

function displayWorkloadsTable(source: string, workloads: WorkloadDescriptor[]): void {
  newline()
  header(`${icons.package} Discovered Workloads from ${source} (${workloads.length})`)

  if (workloads.length === 0) {
    info('No workloads discovered from this source')
    newline()
    return
  }

  const columns: TableColumn<WorkloadDescriptor>[] = [
    { header: 'Name', accessor: (w) => w.name ?? w.id, color: (v) => colors.bold(v) },
    { header: 'Type', key: 'workload_type' },
    { header: 'Status', accessor: (w) => w.status, color: (v) => statusBadge(v) },
    { header: 'Image', accessor: (w) => w.image ?? '', color: (v) => colors.muted(v) },
  ]
  printTable(workloads, columns, { style: 'minimal' })
  newline()
}

// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

async function listSourcesAction(options: SourcesOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const sources = await withSpinner('Fetching import sources...', fetchSources)

  if (options.json) {
    json(sources)
    return
  }

  newline()
  header(`${icons.package} Import Sources (${sources.length})`)

  const columns: TableColumn<ImportSourceInfo>[] = [
    { header: 'Source', accessor: (s) => s.source, color: (v) => colors.bold(v) },
    { header: 'Name', key: 'name' },
    { header: 'Status', accessor: (s) => (s.available ? 'available' : 'unavailable'), color: (v) => statusBadge(v) },
    { header: 'Needs credentials', accessor: (s) => (s.capabilities.requires_credentials ? 'yes' : 'no') },
  ]
  printTable(sources, columns, { style: 'minimal' })
  newline()
}

async function discoverAction(options: DiscoverOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const sourceInfo = await pickSource(options.source)
  const credentials = await resolveCredentials(sourceInfo.source, options)
  const workloads = await discover(sourceInfo.source, credentials)

  if (options.json) {
    json({ workloads })
    return
  }

  displayWorkloadsTable(sourceInfo.source, workloads)
}

async function planAction(options: PlanOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const sourceInfo = await pickSource(options.source)
  const credentials = await resolveCredentials(sourceInfo.source, options)
  const workloads = await discover(sourceInfo.source, credentials)
  if (workloads.length === 0) {
    warning('No workloads discovered from this source')
    return
  }
  const workload = await pickWorkload(workloads, options.workload)

  const planResult = await withSpinner(`Creating import plan for ${workload.name ?? workload.id}...`, async () => {
    const { data, error } = await createPlan({
      client,
      body: { source: sourceInfo.source, workload_id: workload.id, credentials },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data
  })

  if (!planResult) throw new Error('No plan returned')

  displayImportPlan(planResult.plan, planResult.validation)
  keyValue('Session ID', planResult.session_id)
  keyValue('Can execute', planResult.can_execute ? colors.success('yes') : colors.error('no'))
  newline()
  info('To execute this plan, run:')
  info(`  temps migrate execute --session-id ${planResult.session_id} --project-name <name>`)
  newline()
}

async function runAction(options: RunOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const sourceInfo = await pickSourceForMigration(options.source)

  if (sourceInfo.source === 'vercel') {
    // Vercel has no backend importer yet — its migration is a separate,
    // client-side wizard that talks to Vercel's API directly. See
    // `../migrate/index.ts` for why.
    await runVercelMigrationWizard()
    return
  }

  const credentials = await resolveCredentials(sourceInfo.source, options)
  const credDescription = describeCredentials(sourceInfo.source, credentials)
  if (credDescription) {
    info(`Connecting to ${credDescription}`)
  }

  const workloads = await discover(sourceInfo.source, credentials)
  if (workloads.length === 0) {
    warning('No workloads discovered from this source')
    return
  }
  displayWorkloadsTable(sourceInfo.source, workloads)
  const workload = await pickWorkload(workloads, options.workload)

  const planResult = await withSpinner(`Creating import plan for ${workload.name ?? workload.id}...`, async () => {
    const { data, error } = await createPlan({
      client,
      body: { source: sourceInfo.source, workload_id: workload.id, credentials },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data
  })
  if (!planResult) throw new Error('No plan returned')

  displayImportPlan(planResult.plan, planResult.validation)

  if (!planResult.can_execute) {
    printError('Validation failed — this plan cannot be executed. Fix the issues above and try again.')
    return
  }

  if (options.dryRun) {
    success('Dry run only — nothing was created. Re-run without --dry-run to execute for real.')
    return
  }

  if (!options.yes) {
    const confirmed = await promptConfirm({
      message: `Execute this import into a new temps project?`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled — the plan above is not saved after this process exits.')
      return
    }
  }

  const projectName =
    options.projectName ??
    (await promptText({
      message: 'Project name',
      default: planResult.plan.project.name,
      required: true,
    }))

  const defaultPreset = planResult.plan.deployment.git ? 'nixpacks' : 'dockerfile'
  const preset = options.preset ?? defaultPreset

  const result = await withSpinner(
    `Executing import — creating services, deploying, and verifying the app responds (this can take a few minutes)...`,
    async () => {
      const { data, error } = await executeImport({
        client,
        body: {
          session_id: planResult.session_id,
          project_name: projectName,
          directory: options.directory ?? '.',
          main_branch: options.branch ?? planResult.plan.deployment.git?.branch ?? 'main',
          preset,
          dry_run: false,
        },
      })
      if (error) throw new Error(getErrorMessage(error))
      return data
    },
  )
  if (!result) throw new Error('No result returned from execute')

  displayExecuteResult(result)
}

async function executeSessionAction(options: {
  sessionId: string
  projectName?: string
  preset?: string
  directory?: string
  branch?: string
  dryRun?: boolean
  yes?: boolean
}): Promise<void> {
  await requireAuth()
  await setupClient()

  if (!options.yes) {
    warning('This will create real resources (services, a project, a deployment) in temps.')
    const confirmed = await promptConfirm({ message: 'Continue?', default: false })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  const result = await withSpinner('Executing import...', async () => {
    const { data, error } = await executeImport({
      client,
      body: {
        session_id: options.sessionId,
        project_name: options.projectName ?? options.sessionId,
        directory: options.directory ?? '.',
        main_branch: options.branch ?? 'main',
        preset: options.preset ?? 'nixpacks',
        dry_run: options.dryRun ?? false,
      },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data
  })
  if (!result) throw new Error('No result returned from execute')

  displayExecuteResult(result)
}

async function statusAction(options: StatusOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const status = await withSpinner('Fetching import session...', async () => {
    const { data, error } = await getImportStatus({ client, path: { session_id: options.sessionId } })
    if (error || !data) throw new Error(getErrorMessage(error) ?? `Import session ${options.sessionId} not found`)
    return data
  })

  if (options.json) {
    json(status)
    return
  }

  newline()
  header(`${icons.info} Import Session: ${options.sessionId}`)
  keyValue('Source', status.plan?.source ?? 'unknown')
  keyValue('Created', new Date(status.created_at).toLocaleString())
  if (status.plan && status.validation) {
    displayImportPlan(status.plan, status.validation)
  }
  if (status.warnings.length > 0) {
    newline()
    header('Warnings')
    for (const w of status.warnings) console.log(`  ${colors.warning('!')} ${w}`)
  }
  if (status.errors.length > 0) {
    newline()
    header('Errors')
    for (const e of status.errors) console.log(`  ${colors.error('x')} ${e}`)
  }
  newline()
}
