// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// `environments placement|isolation|volumes|ingress`: where an environment's
// replicas run, how they are isolated, the volumes they mount, and whether
// visitors must sign in to Temps (ADR-055).

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { requireProjectSlug } from '../../config/resolve-project.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  adminAttachEnvironmentVolume,
  adminCreateEnvironmentVolume,
  adminDeleteEnvironmentVolume,
  adminDetachEnvironmentVolume,
  adminEnvironmentPinningRecommendation,
  adminEnvironmentVolumeStatus,
  adminExplainEnvironmentPlacement,
  adminGetEnvironmentWorkloadIsolation,
  adminListEnvironmentVolumes,
  adminSetEnvironmentWorkloadIsolation,
  getEnvironments,
  getIngressSettings,
  getProjectBySlug,
  updateIngressSettings,
} from '../../api/sdk.gen.js'
import type {
  CreateEnvironmentVolumeRequest,
  EnvironmentVolumeResponse,
  EnvironmentWorkloadIsolation,
  IngressSettingsResponse,
  PinningRecommendationResponse,
  PlacementExplanationResponse,
  WorkloadIsolationResponse,
} from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable } from '../../ui/table.js'
import { promptConfirm } from '../../ui/prompts.js'
import {
  newline,
  header,
  icons,
  json,
  colors,
  success,
  warning,
  keyValue,
  info,
  error as errorOutput,
} from '../../ui/output.js'

// ============================================================================
// Pure helpers (unit tested)
// ============================================================================

export const WORKLOAD_ISOLATIONS: readonly EnvironmentWorkloadIsolation[] = ['process', 'micro_vm']

/** A `--isolation` value, or an error naming the accepted ones. */
export function parseWorkloadIsolation(
  value: string
): { isolation: EnvironmentWorkloadIsolation } | { error: string } {
  const normalized = value.trim().toLowerCase().replace('-', '_')
  const match = WORKLOAD_ISOLATIONS.find((isolation) => isolation === normalized)
  return match
    ? { isolation: match }
    : { error: `Unknown isolation "${value}": use one of ${WORKLOAD_ISOLATIONS.join(', ')}` }
}

/** A whole number written with digits only, at least `min`. */
function parseWholeNumber(value: string, min: number): number | null {
  const trimmed = value.trim()
  if (!/^\d+$/.test(trimmed)) return null
  const parsed = Number(trimmed)
  return Number.isSafeInteger(parsed) && parsed >= min ? parsed : null
}

/** `--window-days`: 1 to 90 days, or the server default when unset. */
export function parseWindowDays(value: string | undefined): { days?: number } | { error: string } {
  if (value === undefined) return {}
  const days = parseWholeNumber(value, 1)
  if (days === null || days > 90) {
    return { error: `--window-days must be a whole number from 1 to 90 (got "${value}")` }
  }
  return { days }
}

/**
 * Same rule as the server (2-64 characters of `[a-z0-9_.-]`, starting with a
 * letter or digit), so a bad name fails before any request.
 */
export function validVolumeName(name: string): boolean {
  return /^[a-z0-9][a-z0-9_.-]{1,63}$/.test(name)
}

export interface CreateVolumeFlags {
  name?: string
  sizeMb?: string
  retain?: boolean
}

/** The `volumes create` request body, or the first problem with the flags. */
export function buildCreateVolumeRequest(
  flags: CreateVolumeFlags
): { body: CreateEnvironmentVolumeRequest } | { error: string } {
  const name = flags.name?.trim()
  if (!name) return { error: '--name is required' }
  if (!validVolumeName(name)) {
    return {
      error: `Invalid volume name "${name}": use 2-64 characters of a-z, 0-9, "_", "." or "-", starting with a letter or digit`,
    }
  }
  const body: CreateEnvironmentVolumeRequest = { name, retain_on_delete: flags.retain === true }
  if (flags.sizeMb !== undefined) {
    const size = parseWholeNumber(flags.sizeMb, 1)
    if (size === null) return { error: `--size-mb must be a positive whole number (got "${flags.sizeMb}")` }
    body.size_mb = size
  }
  return { body }
}

/** `--mount-path`: an absolute path inside the replica, never `/` itself. */
export function validateMountPath(path: string | undefined): { mountPath: string } | { error: string } {
  const mountPath = path?.trim()
  if (!mountPath) return { error: '--mount-path is required' }
  if (!mountPath.startsWith('/') || mountPath === '/') {
    return { error: `--mount-path must be an absolute path other than "/" (got "${mountPath}")` }
  }
  return { mountPath }
}

/**
 * `ingress set` changes only the login requirement: the ingress mode is a
 * migration switch the CLI deliberately does not offer.
 */
export function resolveRequireLogin(requireLogin: boolean | undefined): { requireLogin: boolean } | { error: string } {
  if (requireLogin === undefined) {
    return { error: 'Pass --require-login or --no-require-login' }
  }
  return { requireLogin }
}

/** A volume argument: its numeric id, or a name to look up. */
export function parseVolumeRef(value: string): { id: number } | { name: string } {
  const id = parseWholeNumber(value, 1)
  return id !== null ? { id } : { name: value.trim() }
}

function describeNode(id: number): string {
  return id === 0 ? 'control plane' : `node ${id}`
}

export function describeAttachment(volume: EnvironmentVolumeResponse): string {
  if (!volume.attachment) return 'detached'
  return `${volume.attachment.mount_path}${volume.attachment.read_only ? ' (read-only)' : ''}`
}

// ============================================================================
// Commander wiring
// ============================================================================

interface TargetOptions {
  project?: string
  json?: boolean
}

export function registerEnvironmentWorkloadCommands(environments: Command): void {
  const placement = environments
    .command('placement')
    .description('Where the scheduler would run an environment\'s replicas, and why')

  placement
    .command('explain <environment>')
    .description('Show the nodes each replica would run on now, the eligible nodes ranked, and the ones excluded with the reason')
    .option('-p, --project <project>', 'Project slug or ID')
    .option('--json', 'Output in JSON format')
    .action(placementExplainCmd)

  placement
    .command('recommend <environment>')
    .description('Recommend target nodes from where the environment actually ran (recommended, never applied)')
    .option('-p, --project <project>', 'Project slug or ID')
    .option('--window-days <days>', 'How many days of history to consider (1-90, default 7)')
    .option('--json', 'Output in JSON format')
    .action(placementRecommendCmd)

  const isolation = environments
    .command('isolation')
    .description(
      'How an environment\'s replicas are isolated: a container (process) or a Firecracker microVM (micro_vm). ' +
        'Honoured only for environments whose deployments run on the workload plane'
    )

  isolation
    .command('show <environment>')
    .description('Show the isolation and whether it takes effect')
    .option('-p, --project <project>', 'Project slug or ID')
    .option('--json', 'Output in JSON format')
    .action(isolationShowCmd)

  isolation
    .command('set <environment> <isolation>')
    .description('Set the isolation: process or micro_vm (a microVM replica only runs on a node that provides one)')
    .option('-p, --project <project>', 'Project slug or ID')
    .option('--json', 'Output in JSON format')
    .action(isolationSetCmd)

  const volumes = environments
    .command('volumes')
    .description('Persistent volumes mounted into an environment\'s replicas; a replica that mounts one is pinned to its node')

  volumes
    .command('list <environment>')
    .alias('ls')
    .description('List the environment\'s volumes, newest first')
    .option('-p, --project <project>', 'Project slug or ID')
    .option('--page <page>', 'Page number (default 1)')
    .option('--page-size <size>', 'Results per page (default 20, max 100)')
    .option('--json', 'Output in JSON format')
    .action(volumesListCmd)

  volumes
    .command('create <environment>')
    .description('Create a volume (attach it to mount it)')
    .option('-p, --project <project>', 'Project slug or ID')
    .option('-n, --name <name>', 'Volume name: 2-64 characters of a-z, 0-9, "_", "." or "-"')
    .option('--size-mb <mb>', 'Size in MB, when the backend supports a size')
    .option('--retain', 'Keep the data when the volume is deleted')
    .option('--json', 'Output in JSON format')
    .action(volumesCreateCmd)

  volumes
    .command('status <environment>')
    .description('Show whether the next deploy mounts the attached volumes, and what to fix if not')
    .option('-p, --project <project>', 'Project slug or ID')
    .option('--json', 'Output in JSON format')
    .action(volumesStatusCmd)

  volumes
    .command('attach <environment> <volume>')
    .description('Mount a volume (id or name) into every replica at --mount-path')
    .option('-p, --project <project>', 'Project slug or ID')
    .option('--mount-path <path>', 'Absolute path inside each replica')
    .option('--read-only', 'Mount it read-only')
    .option('--json', 'Output in JSON format')
    .action(volumesAttachCmd)

  volumes
    .command('detach <environment> <volume>')
    .description('Stop mounting a volume (id or name); its data is kept')
    .option('-p, --project <project>', 'Project slug or ID')
    .option('--json', 'Output in JSON format')
    .action(volumesDetachCmd)

  volumes
    .command('rm <environment> <volume>')
    .alias('delete')
    .description('Delete a volume (id or name), and its data unless it was created with --retain')
    .option('-p, --project <project>', 'Project slug or ID')
    .option('-f, --force', 'Skip confirmation')
    .option('-y, --yes', 'Skip confirmation (alias for --force)')
    .option('--json', 'Output in JSON format')
    .action(volumesRemoveCmd)

  const ingress = environments
    .command('ingress')
    .description('Whether visitors must sign in to Temps to open an environment')

  ingress
    .command('show <environment>')
    .description('Show the login requirement and whether the proxy enforces it now')
    .option('-p, --project <project>', 'Project slug or ID')
    .option('--json', 'Output in JSON format')
    .action(ingressShowCmd)

  ingress
    .command('set <environment>')
    .description('Require visitors to sign in to Temps, or stop requiring it')
    .option('-p, --project <project>', 'Project slug or ID')
    .option('--require-login', 'Visitors must sign in to Temps')
    .option('--no-require-login', 'Visitors do not need to sign in')
    .option('--json', 'Output in JSON format')
    .action(ingressSetCmd)
}

// ============================================================================
// Resolution
// ============================================================================

interface Target {
  projectSlug: string
  projectId: number
  environmentId: number
  environmentSlug: string
}

async function resolveTarget(projectOption: string | undefined, environment: string): Promise<Target> {
  const resolved = await requireProjectSlug(projectOption)
  if (resolved.source !== 'flag') {
    info(`Using project ${colors.bold(resolved.slug)} (from ${resolved.source})`)
  }
  return withSpinner('Resolving environment...', async () => {
    const project = await getProjectBySlug({ client, path: { slug: resolved.slug } })
    if (project.error || !project.data) {
      throw new Error(`Project "${resolved.slug}" not found`)
    }
    const envs = await getEnvironments({ client, path: { project_id: project.data.id } })
    if (envs.error) throw new Error(getErrorMessage(envs.error))
    const list = envs.data ?? []
    const target = list.find(
      (e) => e.slug === environment || e.name.toLowerCase() === environment.toLowerCase()
    )
    if (!target) {
      throw new Error(
        `Environment "${environment}" not found in project "${resolved.slug}". Available: ${list.map((e) => e.slug).join(', ')}`
      )
    }
    return {
      projectSlug: resolved.slug,
      projectId: project.data.id,
      environmentId: target.id,
      environmentSlug: target.slug,
    }
  })
}

async function connect(): Promise<void> {
  await requireAuth()
  await setupClient()
}

function fail(message: string): void {
  errorOutput(message)
  process.exitCode = 1
}

/** A volume's id, looking a name up page by page. */
async function resolveVolumeId(target: Target, ref: string): Promise<number> {
  const parsed = parseVolumeRef(ref)
  if ('id' in parsed) return parsed.id
  for (let page = 1; ; page++) {
    const { data, error } = await adminListEnvironmentVolumes({
      client,
      path: { environment_id: target.environmentId },
      query: { page, page_size: 100 },
    })
    if (error || !data) throw new Error(getErrorMessage(error))
    const match = data.volumes.find((v) => v.name === parsed.name)
    if (match) return match.id
    if (page * data.page_size >= data.total || data.volumes.length === 0) {
      throw new Error(`Volume "${parsed.name}" not found in environment "${target.environmentSlug}"`)
    }
  }
}

// ============================================================================
// Placement
// ============================================================================

async function placementExplainCmd(environment: string, options: TargetOptions): Promise<void> {
  await connect()
  const target = await resolveTarget(options.project, environment)
  const result = await withSpinner('Explaining placement...', async () => {
    const { data, error } = await adminExplainEnvironmentPlacement({
      client,
      path: { environment_id: target.environmentId },
    })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data
  })
  if (options.json) {
    json(result)
    return
  }
  printPlacement(target, result)
}

function printPlacement(target: Target, result: PlacementExplanationResponse): void {
  newline()
  header(`${icons.globe} Placement for ${target.projectSlug}/${target.environmentSlug}`)
  if (result.placement && result.placement.length > 0) {
    keyValue('Replicas would run on', result.placement.map(describeNode).join(', '))
  }
  if (result.pinned_by) keyValue('Pinned', result.pinned_by)
  if (result.unschedulable || result.unschedulable_reason) {
    newline()
    console.log(`  ${colors.error(result.unschedulable ?? 'Unschedulable')}`)
    if (result.unschedulable_reason) console.log(`  ${colors.muted(result.unschedulable_reason)}`)
  }
  if (result.recommended.length > 0) {
    newline()
    console.log(colors.bold('  Eligible nodes (best first)'))
    printTable(result.recommended, [
      { header: 'Node', accessor: (n) => `${n.node_name} (${n.node_id})` },
      { header: 'Score', accessor: (n) => n.score.toFixed(2), align: 'right' },
      { header: 'Why', accessor: (n) => n.reasons.join('; ') },
    ])
  }
  if (result.excluded.length > 0) {
    newline()
    console.log(colors.bold('  Excluded nodes'))
    printTable(result.excluded, [
      { header: 'Node', accessor: (n) => `${n.node_name} (${n.node_id})` },
      { header: 'Reason', accessor: (n) => (n.excluded ? n.reason : `${n.reason} (kept, warning)`) },
      { header: 'Detail', accessor: (n) => n.message },
    ])
  }
  for (const w of result.warnings) warning(w)
  newline()
}

async function placementRecommendCmd(
  environment: string,
  options: TargetOptions & { windowDays?: string }
): Promise<void> {
  const window = parseWindowDays(options.windowDays)
  if ('error' in window) return fail(window.error)
  await connect()
  const target = await resolveTarget(options.project, environment)
  const result = await withSpinner('Computing recommendation...', async () => {
    const { data, error } = await adminEnvironmentPinningRecommendation({
      client,
      path: { environment_id: target.environmentId },
      query: window.days !== undefined ? { window_days: window.days } : undefined,
    })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data
  })
  if (options.json) {
    json(result)
    return
  }
  printRecommendation(target, result)
}

function printRecommendation(target: Target, result: PinningRecommendationResponse): void {
  const nodes = (ids: number[]) => (ids.length > 0 ? ids.map(describeNode).join(', ') : colors.muted('any node'))
  newline()
  header(`${icons.globe} Pinning recommendation for ${target.projectSlug}/${target.environmentSlug}`)
  keyValue('Window', `${result.window_days} days`)
  keyValue('Ran on', nodes(result.nodes_used))
  keyValue('Current target nodes', nodes(result.current_nodes))
  keyValue('Recommended target nodes', nodes(result.recommended_nodes))
  keyValue('Changes placement', result.changes_placement ? colors.warning('yes') : 'no')
  for (const reason of result.reasons) console.log(`  ${colors.muted('•')} ${reason}`)
  newline()
  info('This is a recommendation only; nothing was changed.')
}

// ============================================================================
// Isolation
// ============================================================================

function printIsolation(target: Target, result: WorkloadIsolationResponse): void {
  newline()
  header(`${icons.folder} Isolation for ${target.projectSlug}/${target.environmentSlug}`)
  keyValue('Isolation', result.workload_isolation)
  keyValue(
    'Takes effect',
    result.applies
      ? colors.success('yes')
      : colors.warning('no: this environment\'s deployments do not run on the workload plane yet')
  )
  newline()
}

async function isolationShowCmd(environment: string, options: TargetOptions): Promise<void> {
  await connect()
  const target = await resolveTarget(options.project, environment)
  const result = await withSpinner('Fetching isolation...', async () => {
    const { data, error } = await adminGetEnvironmentWorkloadIsolation({
      client,
      path: { environment_id: target.environmentId },
    })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data
  })
  if (options.json) {
    json(result)
    return
  }
  printIsolation(target, result)
}

async function isolationSetCmd(environment: string, value: string, options: TargetOptions): Promise<void> {
  const parsed = parseWorkloadIsolation(value)
  if ('error' in parsed) return fail(parsed.error)
  await connect()
  const target = await resolveTarget(options.project, environment)
  const result = await withSpinner('Updating isolation...', async () => {
    const { data, error } = await adminSetEnvironmentWorkloadIsolation({
      client,
      path: { environment_id: target.environmentId },
      body: { workload_isolation: parsed.isolation },
    })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data
  })
  if (options.json) {
    json(result)
    return
  }
  success(`Isolation set to ${result.workload_isolation}`)
  printIsolation(target, result)
}

// ============================================================================
// Volumes
// ============================================================================

function printVolumes(volumes: EnvironmentVolumeResponse[]): void {
  printTable(volumes, [
    { header: 'ID', key: 'id' },
    { header: 'Name', key: 'name' },
    { header: 'Mount', accessor: describeAttachment },
    { header: 'Node', accessor: (v) => (v.node_id === null || v.node_id === undefined ? colors.muted('not created') : describeNode(v.node_id)) },
    { header: 'Size', accessor: (v) => (v.size_mb ? `${v.size_mb} MB` : '-') },
    { header: 'Retain', accessor: (v) => (v.retain_on_delete ? 'yes' : 'no') },
  ])
}

async function volumesListCmd(
  environment: string,
  options: TargetOptions & { page?: string; pageSize?: string }
): Promise<void> {
  const page = options.page === undefined ? 1 : parseWholeNumber(options.page, 1)
  if (page === null) return fail(`--page must be a positive whole number (got "${options.page}")`)
  const pageSize = options.pageSize === undefined ? 20 : parseWholeNumber(options.pageSize, 1)
  if (pageSize === null || pageSize > 100) {
    return fail(`--page-size must be a whole number from 1 to 100 (got "${options.pageSize}")`)
  }
  await connect()
  const target = await resolveTarget(options.project, environment)
  const result = await withSpinner('Fetching volumes...', async () => {
    const { data, error } = await adminListEnvironmentVolumes({
      client,
      path: { environment_id: target.environmentId },
      query: { page, page_size: pageSize },
    })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data
  })
  if (options.json) {
    json(result)
    return
  }
  newline()
  header(`${icons.folder} Volumes for ${target.projectSlug}/${target.environmentSlug} (${result.total})`)
  printVolumes(result.volumes)
  if (result.page * result.page_size < result.total) {
    info(`More volumes: --page ${result.page + 1}`)
  }
  newline()
}

async function volumesCreateCmd(environment: string, options: TargetOptions & CreateVolumeFlags): Promise<void> {
  const request = buildCreateVolumeRequest(options)
  if ('error' in request) return fail(request.error)
  await connect()
  const target = await resolveTarget(options.project, environment)
  const volume = await withSpinner(`Creating volume ${request.body.name}...`, async () => {
    const { data, error } = await adminCreateEnvironmentVolume({
      client,
      path: { environment_id: target.environmentId },
      body: request.body,
    })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data
  })
  if (options.json) {
    json(volume)
    return
  }
  success(`Volume ${colors.bold(volume.name)} created (id ${volume.id})`)
  info(
    `Attach it to mount it: bunx @temps-sdk/cli env volumes attach ${target.environmentSlug} ${volume.name} -p ${target.projectSlug} --mount-path /data`
  )
}

async function volumesStatusCmd(environment: string, options: TargetOptions): Promise<void> {
  await connect()
  const target = await resolveTarget(options.project, environment)
  const status = await withSpinner('Checking volumes...', async () => {
    const { data, error } = await adminEnvironmentVolumeStatus({
      client,
      path: { environment_id: target.environmentId },
    })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data
  })
  if (options.json) {
    json(status)
    return
  }
  newline()
  header(`${icons.folder} Volume status for ${target.projectSlug}/${target.environmentSlug}`)
  keyValue('Attached volumes', String(status.attached))
  keyValue('Replicas', String(status.replicas))
  keyValue('Next deploy mounts them', status.active ? colors.success('yes') : colors.error('no'))
  if (status.reason) {
    newline()
    console.log(`  ${colors.error(status.reason)}`)
    if (status.setup_path) console.log(`  ${colors.muted(`Fix it in the console: ${status.setup_path}`)}`)
  }
  newline()
}

async function volumesAttachCmd(
  environment: string,
  volumeRef: string,
  options: TargetOptions & { mountPath?: string; readOnly?: boolean }
): Promise<void> {
  const mount = validateMountPath(options.mountPath)
  if ('error' in mount) return fail(mount.error)
  await connect()
  const target = await resolveTarget(options.project, environment)
  const volume = await withSpinner('Attaching volume...', async () => {
    const volumeId = await resolveVolumeId(target, volumeRef)
    const { data, error } = await adminAttachEnvironmentVolume({
      client,
      path: { environment_id: target.environmentId, volume_id: volumeId },
      body: { mount_path: mount.mountPath, read_only: options.readOnly === true },
    })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data
  })
  if (options.json) {
    json(volume)
    return
  }
  success(`Volume ${colors.bold(volume.name)} mounts at ${describeAttachment(volume)}`)
  if (volume.pin_reason) info(volume.pin_reason)
  info('Takes effect on the next deploy.')
}

async function volumesDetachCmd(environment: string, volumeRef: string, options: TargetOptions): Promise<void> {
  await connect()
  const target = await resolveTarget(options.project, environment)
  const volume = await withSpinner('Detaching volume...', async () => {
    const volumeId = await resolveVolumeId(target, volumeRef)
    const { data, error } = await adminDetachEnvironmentVolume({
      client,
      path: { environment_id: target.environmentId, volume_id: volumeId },
    })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data
  })
  if (options.json) {
    json(volume)
    return
  }
  success(`Volume ${colors.bold(volume.name)} detached; its data is kept`)
  info('Takes effect on the next deploy.')
}

async function volumesRemoveCmd(
  environment: string,
  volumeRef: string,
  options: TargetOptions & { force?: boolean; yes?: boolean }
): Promise<void> {
  await connect()
  const target = await resolveTarget(options.project, environment)
  const volumeId = await withSpinner('Resolving volume...', () => resolveVolumeId(target, volumeRef))
  if (!options.force && !options.yes) {
    const confirmed = await promptConfirm({
      message: `Delete volume ${volumeRef} from ${target.projectSlug}/${target.environmentSlug}? Its data is removed unless it was created with --retain.`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }
  const result = await withSpinner('Deleting volume...', async () => {
    const { data, error } = await adminDeleteEnvironmentVolume({
      client,
      path: { environment_id: target.environmentId, volume_id: volumeId },
    })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data
  })
  if (options.json) {
    json(result)
    return
  }
  success(`Volume ${colors.bold(result.volume.name)} deleted`)
  if (result.backing_removed) {
    info('Its data was removed.')
  } else {
    info(`Its data was kept${result.backing_kept_reason ? `: ${result.backing_kept_reason}` : '.'}`)
  }
}

// ============================================================================
// Ingress login
// ============================================================================

function printIngress(target: Target, settings: IngressSettingsResponse): void {
  newline()
  header(`${icons.globe} Ingress for ${target.projectSlug}/${target.environmentSlug}`)
  keyValue('Require Temps login', settings.require_login ? colors.success('yes') : 'no')
  keyValue('Enforced now', settings.login_enforced ? colors.success('yes') : colors.muted('no'))
  if (settings.login_permission) keyValue('Permission needed', settings.login_permission)
  keyValue('Ingress mode', colors.muted(settings.mode))
  if (settings.require_login && !settings.login_enforced) {
    newline()
    warning(`The login is recorded but not enforced: this environment's ingress mode is "${settings.mode}".`)
  }
  newline()
}

async function ingressShowCmd(environment: string, options: TargetOptions): Promise<void> {
  await connect()
  const target = await resolveTarget(options.project, environment)
  const settings = await withSpinner('Fetching ingress settings...', async () => {
    const { data, error } = await getIngressSettings({
      client,
      path: { project_id: target.projectId, env_id: target.environmentId },
    })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data
  })
  if (options.json) {
    json(settings)
    return
  }
  printIngress(target, settings)
}

async function ingressSetCmd(
  environment: string,
  options: TargetOptions & { requireLogin?: boolean }
): Promise<void> {
  const parsed = resolveRequireLogin(options.requireLogin)
  if ('error' in parsed) return fail(parsed.error)
  await connect()
  const target = await resolveTarget(options.project, environment)
  const settings = await withSpinner('Updating ingress settings...', async () => {
    const { data, error } = await updateIngressSettings({
      client,
      path: { project_id: target.projectId, env_id: target.environmentId },
      body: { require_login: parsed.requireLogin },
    })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data
  })
  if (options.json) {
    json(settings)
    return
  }
  success(parsed.requireLogin ? 'Visitors must now sign in to Temps' : 'Visitors no longer need to sign in')
  printIngress(target, settings)
}
