// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Settings beyond the curated `settings update` flags: any instance setting
 * by field path, the admin listener gate, anonymous telemetry, the CI
 * builder, and the read-only checks and maintenance actions under
 * `/settings` (disk status, public ingress, route table, weekly digest).
 */

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  getAdminGate,
  getCiBuilder,
  getDiskStatus,
  getPublicIngressStatus,
  getSettings,
  getTelemetrySettings,
  patchAdminGate,
  provisionCiBuilder,
  refreshRouteTable,
  setCiBuilder,
  triggerWeeklyDigest,
  updateSettings,
  updateTelemetrySettings,
} from '../../api/sdk.gen.js'
import type { AppSettings, CiExecutorSettings, CiExecutorSettingsWritable, UpdateAdminGateRequest } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { promptConfirm } from '../../ui/prompts.js'
import { newline, header, icons, json, success, info, warning, keyValue } from '../../ui/output.js'
import { ValidationError } from '../../utils/errors.js'
import { resolveValueSource } from '../../lib/value-source.js'
import {
  applyAssignments,
  collect,
  getAtPath,
  isJsonObject,
  parseAssignment,
  parsePath,
  readJsonObject,
  rejectUnknownFields,
  type Assignment,
  type JsonObject,
} from '../../lib/json-edit.js'
import { SENSITIVE_SETTINGS, SETTING_FIELDS } from './fields.js'

interface JsonOptions {
  json?: boolean
}

interface ApplyOptions extends JsonOptions {
  fromFile?: string
  valueStdin?: boolean
  yes?: boolean
}

function fail(what: string, error: unknown): never {
  throw new Error(`${what}: ${getErrorMessage(error)}`)
}

/** Ask before a change the caller flagged as risky, unless `--yes`. */
async function confirmRisk(lines: string[], question: string, yes: boolean | undefined): Promise<boolean> {
  if (lines.length === 0 || yes) return true
  for (const line of lines) warning(line)
  const confirmed = await promptConfirm({ message: question, default: false })
  if (!confirmed) info('Cancelled; nothing was changed.')
  return confirmed
}

/**
 * The assignments `settings apply` makes. A bare path (no `=`) takes its
 * value from stdin, for secrets such as `geo.maxmind_license_key`.
 */
export function settingsAssignments(raw: string[], stdinValue: string | undefined): Assignment[] {
  const bare = raw.filter((entry) => !entry.includes('='))
  if (bare.length > 1) throw new ValidationError(`Only one field can read its value from stdin, got ${bare.join(', ')}`)
  if (bare.length === 1 && stdinValue === undefined) {
    throw new ValidationError(`"${bare[0]}" has no value: write ${bare[0]}=<value>, or pass --value-stdin`)
  }
  if (bare.length === 0 && stdinValue !== undefined) {
    throw new ValidationError('--value-stdin needs a field without "=" to put the value in, e.g. geo.maxmind_license_key')
  }
  return raw.map((entry) =>
    entry.includes('=') ? parseAssignment(entry) : { path: parsePath(entry), value: stdinValue as string },
  )
}

/** Which top-level settings an edit touches, refusing names `AppSettings` does not have. */
export function touchedSections(assignments: Assignment[], document: JsonObject = {}): string[] {
  const sections = new Set([...assignments.map((a) => a.path[0] as string), ...Object.keys(document)])
  for (const section of sections) {
    if (!(section in SETTING_FIELDS)) {
      throw new ValidationError(`Unknown setting "${section}". Run: temps settings fields   to list them`)
    }
  }
  return [...sections].sort()
}

/**
 * The full settings body for PUT /settings: the current settings with the
 * file merged in and the assignments applied. PUT replaces the whole
 * document, so starting from the current one keeps every other setting.
 */
export function mergeSettings(current: JsonObject, fromFile: JsonObject | undefined, assignments: Assignment[]): JsonObject {
  const base = fromFile ? { ...current, ...fromFile } : current
  // Fields absent from the GET response (an unset optional section) may be set.
  return applyAssignments(base, assignments, { allowNew: false })
}

export function registerAdvancedSettingsCommands(settings: Command): void {
  settings
    .command('fields [path]')
    .description('List every instance setting, or print the value at a field path (e.g. shield, otel_ingest.max_spans_per_second)')
    .option('--json', 'Output in JSON format')
    .action(fieldsAction)

  settings
    .command('apply [assignments...]')
    .description(
      'Change any instance setting by field path: field=value (JSON or text), field:=text, or a bare field with --value-stdin',
    )
    .option('--from-file <path>', 'Merge a JSON object of settings from a file (- for stdin)')
    .option('--value-stdin', 'Read the value of the one bare field from stdin (for secrets)')
    .option('-y, --yes', 'Do not ask before changing a setting that can lock you out')
    .option('--json', 'Print the saved settings response as JSON')
    .action(applyAction)

  const gate = settings
    .command('admin-gate')
    .description('Show, or change with flags, which hosts and source IPs may reach the console and API listener')
    .option('--allowed-host <host>', 'Host header to allow (repeatable; replaces the list)', collect)
    .option('--allowed-ip <ip-or-cidr>', 'Source IP or CIDR to allow (repeatable; replaces the list)', collect)
    .option('--clear-hosts', 'Allow any Host header')
    .option('--clear-ips', 'Allow any source address')
    .option('--trust-forwarded-for', 'Trust X-Forwarded-For from loopback peers')
    .option('--no-trust-forwarded-for', 'Ignore X-Forwarded-For')
    .option('-y, --yes', 'Do not ask before saving a gate that may lock you out')
    .option('--json', 'Output in JSON format')
  gate.action(adminGateAction)

  settings
    .command('telemetry')
    .description('Show anonymous telemetry status, or turn it on/off with --enable/--disable')
    .option('--enable', 'Send anonymous telemetry')
    .option('--disable', 'Stop sending anonymous telemetry')
    .option('--json', 'Output in JSON format')
    .action(telemetryAction)

  const ci = settings.command('ci-builder').description('Configure the external CI builder that runs builds off the control plane')
  ci.command('show')
    .description('Show whether the CI builder is configured, and its settings (credentials are never returned)')
    .option('--json', 'Output in JSON format')
    .action(ciShowAction)
  ci.command('set [assignments...]')
    .description('Save CI builder settings from --from-file and/or field=value assignments (e.g. limits.concurrency=2)')
    .option('--from-file <path>', 'JSON object of CI builder settings (- for stdin)')
    .option('--dispatch-token-stdin', 'Read the dispatch token from stdin')
    .option('--json', 'Output in JSON format')
    .action(ciSetAction)
  ci.command('provision')
    .description('Create the builder repository and its pinned runner shim at the configured provider')
    .option('-y, --yes', 'Do not ask for confirmation')
    .action(ciProvisionAction)

  settings
    .command('disk-status')
    .description("Show the control plane's disk usage and alert threshold")
    .option('--json', 'Output in JSON format')
    .action(simpleRead('disk status', () => getDiskStatus({ client })))

  settings
    .command('public-ingress')
    .description('Show where third parties (webhooks, email tracking) call back into Temps, and whether it is reachable')
    .option('--json', 'Output in JSON format')
    .action(simpleRead('public ingress status', () => getPublicIngressStatus({ client })))

  settings
    .command('refresh-routes')
    .description('Rebuild the proxy route table now instead of waiting for the next change')
    .option('--json', 'Output in JSON format')
    .action(simpleRead('route table refresh', () => refreshRouteTable({ client }), 'Route table refreshed'))

  settings
    .command('weekly-digest')
    .description('Generate and send the weekly digest now')
    .option('--json', 'Output in JSON format')
    .action(simpleRead('weekly digest', () => triggerWeeklyDigest({ client }), 'Weekly digest triggered'))
}

/** An action that calls one endpoint and prints its response as fields, or JSON. */
function simpleRead(
  what: string,
  call: () => Promise<{ data?: unknown; error?: unknown }>,
  done?: string,
): (options: JsonOptions) => Promise<void> {
  return async (options) => {
    await requireAuth()
    await setupClient()
    const result = await withSpinner(`Fetching ${what}...`, async () => {
      const { data, error } = await call()
      if (error || data === undefined) fail(`Could not get the ${what}`, error)
      return data
    })
    if (options.json) {
      json(result)
      return
    }
    if (done) success(done)
    printFields(result)
  }
}

/** Print a response object as `key: value` lines, nested objects as JSON. */
export function printFields(value: unknown): void {
  newline()
  if (!isJsonObject(value)) {
    console.log(JSON.stringify(value, null, 2))
    return
  }
  for (const [key, field] of Object.entries(value)) {
    keyValue(key, field === null || typeof field !== 'object' ? (field as string | number | boolean | null) : JSON.stringify(field))
  }
  newline()
}

async function readCurrentSettings(): Promise<JsonObject> {
  const { data, error } = await getSettings({ client })
  if (error || !data) fail('Could not read the instance settings', error)
  return data as unknown as JsonObject
}

async function fieldsAction(path: string | undefined, options: JsonOptions): Promise<void> {
  if (path === undefined) {
    if (options.json) {
      json(SETTING_FIELDS)
      return
    }
    newline()
    header(`${icons.info} Instance settings (${Object.keys(SETTING_FIELDS).length})`)
    for (const [field, description] of Object.entries(SETTING_FIELDS)) keyValue(field, description)
    info('Print one: temps settings fields shield    Change one: temps settings apply shield.enabled=true')
    newline()
    return
  }
  const segments = parsePath(path)
  if (!((segments[0] as string) in SETTING_FIELDS)) {
    throw new ValidationError(`Unknown setting "${segments[0]}". Run: temps settings fields   to list them`)
  }
  await requireAuth()
  await setupClient()
  const current = await withSpinner('Fetching settings...', readCurrentSettings)
  const value = getAtPath(current, segments)
  if (value === undefined) throw new ValidationError(`The settings have no field "${path}"`)
  if (options.json || typeof value === 'object') json(value)
  else console.log(String(value))
}

async function applyAction(raw: string[] | undefined, options: ApplyOptions): Promise<void> {
  let stdinValue: string | undefined
  if (options.valueStdin) {
    if (options.fromFile === '-') throw new ValidationError('--from-file - and --value-stdin both read stdin; use one')
    stdinValue = (
      await resolveValueSource(
        { stdin: true },
        { subject: 'the setting value', flags: { inline: '', file: '', stdin: '--value-stdin' }, rejectEmpty: true },
      )
    )?.value
  }
  const assignments = settingsAssignments(raw ?? [], stdinValue)
  const fromFile = options.fromFile ? await readJsonObject(options.fromFile, 'The settings file') : undefined
  if (fromFile) rejectUnknownFields(fromFile, SETTING_FIELDS, 'The settings file')
  if (!assignments.length && !fromFile) {
    throw new ValidationError('Nothing to change: pass field=value assignments or --from-file. List fields with: temps settings fields')
  }
  const sections = touchedSections(assignments, fromFile)
  await requireAuth()
  await setupClient()
  const current = await withSpinner('Fetching settings...', readCurrentSettings)
  const body = mergeSettings(current, fromFile, assignments)
  const risky = sections.filter((section) => SENSITIVE_SETTINGS.has(section))
  const confirmed = await confirmRisk(
    risky.map((section) => `${section}: ${SETTING_FIELDS[section as keyof typeof SETTING_FIELDS]}. A wrong value can lock you out of the console.`),
    `Save ${risky.join(', ')}?`,
    options.yes,
  )
  if (!confirmed) return
  const result = await withSpinner('Saving settings...', async () => {
    const { data, error } = await updateSettings({ client, body: body as unknown as AppSettings })
    if (error) fail(`Could not save ${sections.join(', ')}`, error)
    return data
  })
  if (options.json) {
    json(result ?? {})
    return
  }
  success(`Saved ${sections.join(', ')}`)
}

export interface AdminGateOptions extends JsonOptions {
  allowedHost?: string[]
  allowedIp?: string[]
  clearHosts?: boolean
  clearIps?: boolean
  trustForwardedFor?: boolean
  yes?: boolean
}

/** The gate to save from flags over the current gate, or `null` when no change flag was given. */
export function buildAdminGate(
  current: UpdateAdminGateRequest,
  options: AdminGateOptions,
): UpdateAdminGateRequest | null {
  if (options.allowedHost && options.clearHosts) throw new ValidationError('Pass --allowed-host or --clear-hosts, not both')
  if (options.allowedIp && options.clearIps) throw new ValidationError('Pass --allowed-ip or --clear-ips, not both')
  const changed =
    options.allowedHost || options.allowedIp || options.clearHosts || options.clearIps || options.trustForwardedFor !== undefined
  if (!changed) return null
  return {
    allowed_hosts: options.clearHosts ? [] : (options.allowedHost ?? current.allowed_hosts),
    allowed_ips: options.clearIps ? [] : (options.allowedIp ?? current.allowed_ips),
    trust_forwarded_for: options.trustForwardedFor ?? current.trust_forwarded_for,
  }
}

async function adminGateAction(options: AdminGateOptions): Promise<void> {
  await requireAuth()
  await setupClient()
  const { data: current, error: readError } = await getAdminGate({ client })
  if (readError || !current) fail('Could not read the admin gate', readError)
  const next = buildAdminGate(current, options)
  if (!next) {
    if (options.json) {
      json(current)
      return
    }
    newline()
    header(`${icons.info} Admin gate (${current.source})`)
    keyValue('Allowed hosts', current.allowed_hosts.length ? current.allowed_hosts.join(', ') : 'any')
    keyValue('Allowed IPs', current.allowed_ips.length ? current.allowed_ips.join(', ') : 'any')
    keyValue('Trust X-Forwarded-For', current.trust_forwarded_for)
    keyValue('Editable', current.editable)
    if (!current.editable) info('Environment variables set this gate, so it cannot be changed here.')
    newline()
    return
  }
  if (!current.editable) {
    throw new ValidationError('Environment variables set the admin gate on this server, so it cannot be changed through the API')
  }
  const confirmed = await confirmRisk(
    ['The gate decides who reaches the console and API. A list that leaves out your own host or address locks you out.'],
    'Save the admin gate?',
    options.yes,
  )
  if (!confirmed) return
  const saved = await withSpinner('Saving the admin gate...', async () => {
    const { data, error } = await patchAdminGate({ client, body: next })
    if (error || !data) fail('Could not save the admin gate', error)
    return data
  })
  if (options.json) json(saved)
  else success('Admin gate saved')
}

async function telemetryAction(options: JsonOptions & { enable?: boolean; disable?: boolean }): Promise<void> {
  if (options.enable && options.disable) throw new ValidationError('Pass --enable or --disable, not both')
  await requireAuth()
  await setupClient()
  const change = options.enable ? true : options.disable ? false : undefined
  const status = await withSpinner('Telemetry...', async () => {
    const { data, error } =
      change === undefined
        ? await getTelemetrySettings({ client })
        : await updateTelemetrySettings({ client, body: { enabled: change } })
    if (error || !data) fail('Could not read or save the telemetry setting', error)
    return data
  })
  if (options.json) {
    json(status)
    return
  }
  if (change !== undefined) success(`Anonymous telemetry ${change ? 'enabled' : 'disabled'}`)
  printFields(status)
}

/** Every CI builder setting; a field added to the API fails the typecheck until listed. */
export const CI_BUILDER_FIELDS = {
  control_plane_url: 'URL the runner reaches this control plane on',
  dispatch_token: 'Token used to dispatch builds (write-only; use --dispatch-token-stdin)',
  git_ref: 'Git ref of the builder workflow',
  identity: 'Trusted OIDC identity issuer (issuer, jwks_url, audience, subject)',
  image_repository: 'Repository built images are pushed to',
  limits: 'Build limits (build_timeout_seconds, concurrency, queue_wait_seconds)',
  organization: 'Whether owner is an organization',
  owner: 'Owner of the builder repository',
  registry_credentials: 'Registry push/pull credentials (write-only)',
  repository: 'Builder repository name',
  runner_archive_sha256: 'SHA-256 of the runner archive',
  runner_archive_url: 'URL of the runner archive',
} as const satisfies Record<keyof CiExecutorSettingsWritable, string>

async function ciShowAction(options: JsonOptions): Promise<void> {
  await requireAuth()
  await setupClient()
  const { data, error } = await getCiBuilder({ client })
  if (error || !data) fail('Could not read the CI builder status', error)
  if (options.json) {
    json(data)
    return
  }
  newline()
  header(`${icons.info} CI builder`)
  keyValue('Configured', data.configured)
  if (data.reason) keyValue('Reason', data.reason)
  if (data.settings) for (const [k, v] of Object.entries(data.settings)) keyValue(k, typeof v === 'object' ? JSON.stringify(v) : v)
  if (!data.configured) info(`Configure it with: temps settings ci-builder set --from-file builder.json   (console: ${data.setup_path})`)
  newline()
}

async function ciSetAction(raw: string[] | undefined, options: JsonOptions & { fromFile?: string; dispatchTokenStdin?: boolean }): Promise<void> {
  if (options.fromFile === '-' && options.dispatchTokenStdin) {
    throw new ValidationError('--from-file - and --dispatch-token-stdin both read stdin; use one')
  }
  const assignments = (raw ?? []).map(parseAssignment)
  const fromFile = options.fromFile ? await readJsonObject(options.fromFile, 'The CI builder file') : undefined
  if (fromFile) rejectUnknownFields(fromFile, CI_BUILDER_FIELDS, 'The CI builder file')
  for (const a of assignments) {
    if (!((a.path[0] as string) in CI_BUILDER_FIELDS)) {
      throw new ValidationError(`Unknown CI builder field "${a.path[0]}". Known: ${Object.keys(CI_BUILDER_FIELDS).join(', ')}`)
    }
  }
  const token = options.dispatchTokenStdin
    ? (await resolveValueSource({ stdin: true }, { subject: 'the dispatch token', flags: { inline: '', file: '', stdin: '--dispatch-token-stdin' }, rejectEmpty: true, newlineHint: true }))?.value
    : undefined
  if (!assignments.length && !fromFile && token === undefined) {
    throw new ValidationError('Nothing to save: pass --from-file, field=value assignments or --dispatch-token-stdin')
  }
  await requireAuth()
  await setupClient()
  const { data: status, error: readError } = await getCiBuilder({ client })
  if (readError || !status) fail('Could not read the CI builder settings', readError)
  const base: JsonObject = { ...((status.settings ?? {}) as unknown as JsonObject), ...(fromFile ?? {}) }
  let body = applyAssignments(base, assignments, { allowNew: true })
  if (token !== undefined) body = { ...body, dispatch_token: token }
  const saved = await withSpinner('Saving the CI builder...', async () => {
    const { data, error } = await setCiBuilder({ client, body: body as unknown as CiExecutorSettings })
    if (error || !data) fail('Could not save the CI builder settings', error)
    return data
  })
  if (options.json) json(saved)
  else success(saved.configured ? 'CI builder saved and configured' : `CI builder saved; not ready: ${saved.reason ?? 'see status'}`)
}

async function ciProvisionAction(options: { yes?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()
  const confirmed = await confirmRisk(
    ['Provisioning creates or updates the builder repository and its runner shim at your provider.'],
    'Provision the CI builder?',
    options.yes,
  )
  if (!confirmed) return
  await withSpinner('Provisioning the CI builder...', async () => {
    const { error } = await provisionCiBuilder({ client })
    if (error) fail('Could not provision the CI builder', error)
  })
  success('CI builder provisioned')
}
