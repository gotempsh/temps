// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  deleteEnvironmentIpAccess,
  deleteProjectIpAccess,
  getEnvironmentIpAccess,
  getProjectIpAccess,
  putEnvironmentIpAccess,
  putProjectIpAccess,
  testEnvironmentIpAccess,
  testProjectIpAccess,
} from '../../api/sdk.gen.js'
import type { IpAccessCidrEntry } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { promptConfirm } from '../../ui/prompts.js'
import { newline, header, icons, json, colors, success, info, keyValue } from '../../ui/output.js'

const MODES = ['disabled', 'allow', 'deny'] as const
type Mode = (typeof MODES)[number]

/** Tailnet ranges, same presets as the console. */
export const TAILSCALE_CIDRS: IpAccessCidrEntry[] = [
  { cidr: '100.64.0.0/10', description: 'Tailscale (IPv4)' },
  { cidr: 'fd7a:115c:a1e0::/48', description: 'Tailscale (IPv6)' },
]

export function parseMode(raw: string): Mode | undefined {
  return (MODES as readonly string[]).includes(raw) ? (raw as Mode) : undefined
}

export function parseId(raw: string | undefined, label: string): number {
  const value = Number(raw)
  if (!raw || !Number.isInteger(value) || value <= 0) {
    throw new Error(`${label} must be a positive integer, got "${raw ?? ''}"`)
  }
  return value
}

/**
 * Merge `--cidr` values with the optional Tailscale preset, trimmed and
 * de-duplicated. Syntax and host-bit validation stay with the server, which
 * is the single source of truth for them.
 */
export function buildCidrList(cidrs: string[], tailscale: boolean): IpAccessCidrEntry[] {
  const seen = new Set<string>()
  const list: IpAccessCidrEntry[] = []
  const push = (entry: IpAccessCidrEntry) => {
    const key = entry.cidr.trim().toLowerCase()
    if (key.length === 0 || seen.has(key)) return
    seen.add(key)
    list.push({ cidr: entry.cidr.trim(), description: entry.description ?? null })
  }
  cidrs.flatMap((value) => value.split(',')).forEach((cidr) => push({ cidr }))
  if (tailscale) TAILSCALE_CIDRS.forEach(push)
  return list
}

interface ScopeOptions {
  projectId?: string
  environmentId?: string
}

interface ShowOptions extends ScopeOptions {
  json?: boolean
}

export interface SetOptions extends ScopeOptions {
  mode?: string
  cidr?: string[]
  tailscale?: boolean
  acknowledgeLockout?: boolean
  json?: boolean
}

interface ClearOptions extends ScopeOptions {
  yes?: boolean
}

interface TestOptions extends ScopeOptions {
  ip?: string
  json?: boolean
}

function scopeOf(options: ScopeOptions): { projectId: number; environmentId?: number } {
  const projectId = parseId(options.projectId, '--project-id')
  const environmentId =
    options.environmentId === undefined ? undefined : parseId(options.environmentId, '--environment-id')
  return { projectId, environmentId }
}

function scopeLabel(scope: { projectId: number; environmentId?: number }): string {
  return scope.environmentId === undefined
    ? `project ${scope.projectId}`
    : `environment ${scope.environmentId} of project ${scope.projectId}`
}

function collect(value: string, previous: string[]): string[] {
  return [...previous, value]
}

export function registerProjectIpAccessCommands(ipAccess: Command): void {
  const project = ipAccess
    .command('project')
    .description('Allow or deny IP ranges for one project or environment')

  const withScope = (command: Command): Command =>
    command
      .requiredOption('--project-id <id>', 'Project ID')
      .option('--environment-id <id>', 'Environment ID (omit for the project-wide list)')

  withScope(project.command('show').description('Show the IP access list'))
    .option('--json', 'Output in JSON format')
    .action(showAction)

  withScope(
    project
      .command('set')
      .description('Replace the IP access list. A list that would lock out this machine needs --acknowledge-lockout'),
  )
    .requiredOption('--mode <mode>', 'disabled, allow (only listed) or deny (block listed)')
    .option('--cidr <cidr>', 'Address or CIDR range; repeat or comma-separate', collect, [] as string[])
    .option('--tailscale', 'Include the Tailscale ranges (100.64.0.0/10 and fd7a:115c:a1e0::/48)')
    .option('--acknowledge-lockout', 'Apply even if it would block the address you are calling from')
    .option('--json', 'Output in JSON format')
    .action(setAction)

  withScope(project.command('clear').description('Remove the list (an environment then follows the project list)'))
    .option('-y, --yes', 'Skip confirmation')
    .action(clearAction)

  withScope(project.command('test').description('Check whether an address would be allowed'))
    .requiredOption('--ip <ip>', 'Address to evaluate')
    .option('--json', 'Output in JSON format')
    .action(testAction)
}

async function showAction(options: ShowOptions): Promise<void> {
  await requireAuth()
  await setupClient()
  const scope = scopeOf(options)

  const policy = await withSpinner(`Fetching IP access list for ${scopeLabel(scope)}...`, async () => {
    const { data, error } =
      scope.environmentId === undefined
        ? await getProjectIpAccess({ client, path: { project_id: scope.projectId } })
        : await getEnvironmentIpAccess({
            client,
            path: { project_id: scope.projectId, environment_id: scope.environmentId },
          })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data
  })

  if (options.json) {
    json(policy)
    return
  }

  newline()
  header(`${icons.info} IP access list: ${scopeLabel(scope)}`)
  keyValue('Mode', policy.mode)
  keyValue('Configured', policy.configured ? 'yes' : scope.environmentId === undefined ? 'no (open)' : 'no (follows project)')
  keyValue('Entries', `${policy.cidrs.length} / ${policy.max_cidrs}`)
  policy.cidrs.forEach((entry) => {
    info(`${colors.bold(entry.cidr)}${entry.description ? colors.muted(`  ${entry.description}`) : ''}`)
  })
  if (policy.your_ip) keyValue('Your address', policy.your_ip)
  newline()
}

/**
 * The mode and CIDR list a `set` invocation asks for. Throws (so the process
 * exits non-zero, which scripts rely on) for input the server would only
 * reject later or, worse for an allow list, accept as "block everyone".
 */
export function resolveSetInput(options: SetOptions): {
  mode: Mode
  cidrs: ReturnType<typeof buildCidrList>
} {
  const mode = parseMode(options.mode ?? '')
  if (mode === undefined) {
    throw new Error(`Invalid mode: ${options.mode}. Available: ${MODES.join(', ')}`)
  }
  const cidrs = buildCidrList(options.cidr ?? [], options.tailscale === true)
  if (mode === 'allow' && cidrs.length === 0) {
    throw new Error('An allow list needs at least one --cidr (or --tailscale); otherwise nobody could reach it')
  }
  return { mode, cidrs }
}

async function setAction(options: SetOptions): Promise<void> {
  await requireAuth()
  await setupClient()
  const scope = scopeOf(options)

  const { mode, cidrs } = resolveSetInput(options)

  const saved = await withSpinner(`Saving IP access list for ${scopeLabel(scope)}...`, async () => {
    const body = { mode, cidrs, acknowledge_lockout: options.acknowledgeLockout === true }
    const { data, error } =
      scope.environmentId === undefined
        ? await putProjectIpAccess({ client, path: { project_id: scope.projectId }, body })
        : await putEnvironmentIpAccess({
            client,
            path: { project_id: scope.projectId, environment_id: scope.environmentId },
            body,
          })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data
  })

  if (options.json) {
    json(saved)
    return
  }
  success(`IP access list saved for ${scopeLabel(scope)} (${saved.mode}, ${saved.cidrs.length} entries)`)
  if (saved.mode !== 'disabled') info('Changes reach the proxy within a few seconds')
}

async function clearAction(options: ClearOptions): Promise<void> {
  await requireAuth()
  await setupClient()
  const scope = scopeOf(options)

  if (!options.yes) {
    const confirmed = await promptConfirm({
      message: `Remove the IP access list for ${scopeLabel(scope)}?`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  await withSpinner(`Removing IP access list for ${scopeLabel(scope)}...`, async () => {
    const { error } =
      scope.environmentId === undefined
        ? await deleteProjectIpAccess({ client, path: { project_id: scope.projectId } })
        : await deleteEnvironmentIpAccess({
            client,
            path: { project_id: scope.projectId, environment_id: scope.environmentId },
          })
    if (error) throw new Error(getErrorMessage(error))
  })
  success(`IP access list removed for ${scopeLabel(scope)}`)
}

async function testAction(options: TestOptions): Promise<void> {
  await requireAuth()
  await setupClient()
  const scope = scopeOf(options)
  const ip = (options.ip ?? '').trim()
  if (ip.length === 0) {
    throw new Error('--ip is required')
  }

  const result = await withSpinner(`Evaluating ${ip}...`, async () => {
    const body = { ip }
    const { data, error } =
      scope.environmentId === undefined
        ? await testProjectIpAccess({ client, path: { project_id: scope.projectId }, body })
        : await testEnvironmentIpAccess({
            client,
            path: { project_id: scope.projectId, environment_id: scope.environmentId },
            body,
          })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data
  })

  if (options.json) {
    json(result)
    return
  }
  newline()
  header(`${icons.info} ${result.ip}: ${result.allowed ? colors.success('allowed') : colors.error('blocked')}`)
  info(result.reason)
  newline()
}
