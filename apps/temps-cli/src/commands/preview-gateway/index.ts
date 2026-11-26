// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps preview-gateway`: the container pair that serves workspace preview
 * URLs (`ws-<sid>-<port>.<preview_domain>`). `temps serve` reconciles it on
 * startup; these commands read its status, settings and logs, change its
 * settings, and restart or upgrade it, like Settings > Preview gateway in the
 * console.
 */

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  getPreviewGatewayLogs,
  getPreviewGatewaySettings,
  getPreviewGatewayStatus,
  patchPreviewGatewaySettings,
  restartPreviewGateway,
  upgradePreviewGateway,
} from '../../api/sdk.gen.js'
import type {
  GatewayStatus,
  PatchSettingsRequest,
  PreviewGatewaySettingsResponse,
} from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { promptConfirm } from '../../ui/prompts.js'
import { newline, header, icons, json, colors, success, info, warning, keyValue } from '../../ui/output.js'
import { ValidationError } from '../../utils/errors.js'

interface JsonOptions {
  json?: boolean
}

interface LogsOptions extends JsonOptions {
  tail?: string
}

export interface ConfigureOptions extends JsonOptions {
  enable?: boolean
  disable?: boolean
  image?: string
  resetImage?: boolean
  hostPort?: string
  autoUpgrade?: boolean
  containerName?: string
  resetContainerName?: boolean
  yes?: boolean
}

interface ConfirmOptions {
  yes?: boolean
}

interface UpgradeOptions extends ConfirmOptions {
  image?: string
  default?: boolean
}

/** The server caps `--tail` at 2000 lines. */
export const MAX_LOG_LINES = 2000

/** Parse `--tail`: a whole number of lines, 1 to 2000. */
export function parseTail(raw: string | undefined): number | undefined {
  if (raw === undefined) return undefined
  const value = Number(raw)
  if (!/^\d+$/.test(raw.trim()) || !Number.isSafeInteger(value) || value < 1 || value > MAX_LOG_LINES) {
    throw new ValidationError(`--tail must be a whole number of lines from 1 to ${MAX_LOG_LINES}, got "${raw}"`)
  }
  return value
}

/** Parse `--host-port`: a TCP port, 1 to 65535. */
export function parseHostPort(raw: string): number {
  const value = Number(raw)
  if (!/^\d+$/.test(raw.trim()) || !Number.isSafeInteger(value) || value < 1 || value > 65535) {
    throw new ValidationError(`--host-port must be a TCP port from 1 to 65535, got "${raw}"`)
  }
  return value
}

export interface PreviewGatewayPatch {
  body: PatchSettingsRequest
  /** Why the change interrupts preview URLs, one line each; empty when it does not. */
  disruptions: string[]
}

/**
 * The PATCH body for `preview-gateway configure`, and what in it interrupts
 * preview traffic (so the command can ask first). Only the flags given are
 * sent: every field of the request is optional and an absent one is left as
 * it is.
 */
export function buildPreviewGatewayPatch(
  options: ConfigureOptions,
  current?: Pick<PreviewGatewaySettingsResponse, 'enabled' | 'host_port'>,
): PreviewGatewayPatch {
  if (options.enable && options.disable) {
    throw new ValidationError('Pass --enable or --disable, not both')
  }
  if (options.image !== undefined && options.resetImage) {
    throw new ValidationError('Pass --image or --reset-image, not both')
  }
  if (options.containerName !== undefined && options.resetContainerName) {
    throw new ValidationError('Pass --container-name or --reset-container-name, not both')
  }
  const body: PatchSettingsRequest = {}
  const disruptions: string[] = []
  if (options.enable) body.enabled = true
  if (options.disable) {
    body.enabled = false
    disruptions.push('Disabling removes the gateway containers at once: workspace preview URLs stop being served.')
  }
  if (options.image !== undefined) {
    const image = options.image.trim()
    if (!image) throw new ValidationError('--image needs an image reference; use --reset-image for the default')
    body.image = image
  }
  // An empty image resets the gateway to the image pinned in this Temps build.
  if (options.resetImage) body.image = ''
  if (options.hostPort !== undefined) {
    body.host_port = parseHostPort(options.hostPort)
    // A running gateway is removed and recreated, with its ingress, on the new port.
    // Without the current settings, assume it may be running.
    if (body.enabled !== false && (current === undefined || (current.enabled && current.host_port !== body.host_port))) {
      disruptions.push(
        'Changing the host port removes and recreates the ingress and gateway containers: ' +
          'preview URLs on the old port stop being served until they are back.',
      )
    }
  }
  if (options.autoUpgrade !== undefined) body.auto_upgrade = options.autoUpgrade
  if (options.containerName !== undefined) {
    const name = options.containerName.trim()
    if (!name) throw new ValidationError('--container-name needs a name; use --reset-container-name for the default')
    body.container_name = name
  }
  // An empty container name resets it to the default.
  if (options.resetContainerName) body.container_name = ''
  if (body.container_name !== undefined) {
    disruptions.push(
      'Renaming removes the gateway under its current name and recreates it under the new one; ' +
        'previews are unavailable until it is back.',
    )
  }
  if (Object.keys(body).length === 0) {
    throw new ValidationError(
      'Nothing to change. Pass at least one of --enable, --disable, --image, --reset-image, --host-port, ' +
        '--auto-upgrade, --no-auto-upgrade, --container-name or --reset-container-name',
    )
  }
  return { body, disruptions }
}

/** Ask before an operation that interrupts previews, unless `--yes` was given. */
async function confirmDisruption(lines: string[], question: string, options: ConfirmOptions): Promise<boolean> {
  if (lines.length === 0 || options.yes) return true
  for (const line of lines) warning(line)
  const confirmed = await promptConfirm({ message: question, default: false })
  if (!confirmed) info('Cancelled; nothing was changed.')
  return confirmed
}

export function registerPreviewGatewayCommands(program: Command): void {
  const gateway = program
    .command('preview-gateway')
    .alias('pgw')
    .description('Manage the preview gateway that serves workspace preview URLs (status, settings, logs, restart, upgrade)')

  gateway
    .command('status')
    .description("Show the gateway container's health, image and last setup error")
    .option('--json', 'Output in JSON format')
    .action(statusAction)

  gateway
    .command('settings')
    .alias('show')
    .description('Show the gateway settings: enabled, image, host port, auto-upgrade, container name, and their defaults')
    .option('--json', 'Output in JSON format')
    .action(settingsAction)

  gateway
    .command('configure')
    .alias('set')
    .description('Change gateway settings; only the flags you pass are changed')
    .option('--enable', 'Run the gateway (creates its containers)')
    .option('--disable', 'Stop running the gateway (removes its containers; preview URLs stop being served)')
    .option('--image <ref>', 'Gateway image to run, ideally an immutable @sha256 reference')
    .option('--reset-image', 'Go back to the image pinned in this Temps build')
    .option('--host-port <port>', "Host port the ingress relay's :8080 is published on")
    .option('--auto-upgrade', 'Apply the pinned image on every Temps startup (default)')
    .option('--no-auto-upgrade', 'Leave the running image alone on startup; upgrade by hand')
    .option(
      '--container-name <name>',
      'Docker container name; change only when several Temps instances share one Docker daemon',
    )
    .option('--reset-container-name', 'Go back to the default container name')
    .option('-y, --yes', 'Do not ask before a change that interrupts previews (--disable, a new container name)')
    .option('--json', 'Print the saved settings as JSON')
    .action(configureAction)

  gateway
    .command('logs')
    .description('Print the latest gateway container log lines')
    .option('--tail <lines>', `Lines to print, 1 to ${MAX_LOG_LINES} (default 200)`)
    .option('--json', 'Output in JSON format')
    .action(logsAction)

  gateway
    .command('restart')
    .description('Restart the gateway container (previews are briefly unavailable)')
    .option('-y, --yes', 'Do not ask for confirmation')
    .action(restartAction)

  gateway
    .command('upgrade')
    .description('Pull an image and recreate the gateway with it (previews are briefly unavailable)')
    .option('--image <ref>', 'Image to pull and run, ideally an immutable @sha256 reference')
    .option('--default', 'Use the image pinned in this Temps build')
    .option('-y, --yes', 'Do not ask for confirmation')
    .action(upgradeAction)
}

async function statusAction(options: JsonOptions): Promise<void> {
  await requireAuth()
  await setupClient()
  const status = await withSpinner('Fetching preview gateway status...', async () => {
    const { data, error } = await getPreviewGatewayStatus({ client })
    if (error || !data) throw new Error(`Could not read the preview gateway status: ${getErrorMessage(error)}`)
    return data
  })
  if (options.json) {
    json(status)
    return
  }
  printStatus(status)
}

export function healthColor(health: string): string {
  if (health === 'running') return colors.success(health)
  if (health === 'restarting') return colors.warning(health)
  return colors.error(health)
}

function printStatus(status: GatewayStatus): void {
  newline()
  header(`${icons.info} Preview gateway`)
  keyValue('Health', healthColor(status.health))
  keyValue('Container', status.container_name)
  keyValue('Present', status.present)
  keyValue('Running', status.running)
  keyValue('Started', status.started_at ?? '-')
  keyValue('Image', status.image ?? '-')
  keyValue('Image digest', status.image_digest ?? '-')
  keyValue('Expected image', status.expected_image)
  keyValue('Drift', status.drift ? colors.warning('yes, the running image is not the expected one') : 'no')
  keyValue('Auto-upgrade', status.auto_upgrade)
  keyValue('Host port', status.host_port ?? '-')
  keyValue('Network', status.network ?? '-')
  keyValue('Restarts', status.restart_count ?? '-')
  if (status.last_exit_code !== null && status.last_exit_code !== undefined) {
    keyValue('Last exit code', status.last_exit_code)
  }
  if (status.last_error) keyValue('Last error', colors.error(status.last_error))
  if (status.setup_error) {
    keyValue('Setup error', colors.error(status.setup_error))
    keyValue('Setup failed at', status.setup_failed_at ?? '-')
    keyValue('Next attempt', status.next_setup_attempt_at ?? 'not scheduled')
  }
  if (status.drift) info('Run: temps preview-gateway upgrade --default   to run the expected image')
  if (!status.present) info('Run: temps preview-gateway logs   or check `setup_error` above for why it is missing')
  newline()
}

async function readSettings(): Promise<PreviewGatewaySettingsResponse> {
  const { data, error } = await getPreviewGatewaySettings({ client })
  if (error || !data) throw new Error(`Could not read the preview gateway settings: ${getErrorMessage(error)}`)
  return data
}

function printSettings(settings: PreviewGatewaySettingsResponse): void {
  const marker = (current: unknown, fallback: unknown) => (current === fallback ? colors.muted(' (default)') : '')
  newline()
  header(`${icons.info} Preview gateway settings`)
  keyValue('Enabled', settings.enabled)
  keyValue('Image', `${settings.image}${marker(settings.image, settings.default_image)}`)
  if (settings.image !== settings.default_image) keyValue('Default image', settings.default_image)
  keyValue('Host port', `${settings.host_port}${marker(settings.host_port, settings.default_host_port)}`)
  if (settings.host_port !== settings.default_host_port) keyValue('Default host port', settings.default_host_port)
  keyValue('Auto-upgrade', settings.auto_upgrade)
  keyValue(
    'Container name',
    `${settings.container_name}${marker(settings.container_name, settings.default_container_name)}`,
  )
  if (settings.container_name !== settings.default_container_name) {
    keyValue('Default container name', settings.default_container_name)
  }
  if (!settings.enabled) info('Workspace preview URLs are not served. Run: temps preview-gateway configure --enable')
  newline()
}

async function settingsAction(options: JsonOptions): Promise<void> {
  await requireAuth()
  await setupClient()
  const settings = await withSpinner('Fetching preview gateway settings...', readSettings)
  if (options.json) {
    json(settings)
    return
  }
  printSettings(settings)
}

async function configureAction(options: ConfigureOptions): Promise<void> {
  // Validate the flags before any request.
  buildPreviewGatewayPatch(options)
  await requireAuth()
  await setupClient()
  // Whether a port change interrupts previews depends on the gateway running now.
  const current = options.hostPort !== undefined ? await readSettings() : undefined
  const { body, disruptions } = buildPreviewGatewayPatch(options, current)
  if (!(await confirmDisruption(disruptions, 'Apply this change to the preview gateway?', options))) return
  const settings = await withSpinner('Saving preview gateway settings...', async () => {
    const { data, error } = await patchPreviewGatewaySettings({ client, body })
    if (error || !data) throw new Error(`Could not save the preview gateway settings: ${getErrorMessage(error)}`)
    return data
  })
  if (options.json) {
    json(settings)
    return
  }
  success('Preview gateway settings saved')
  printSettings(settings)
}

async function logsAction(options: LogsOptions): Promise<void> {
  const tail = parseTail(options.tail)
  await requireAuth()
  await setupClient()
  const logs = await withSpinner('Fetching preview gateway logs...', async () => {
    const { data, error } = await getPreviewGatewayLogs({ client, query: tail === undefined ? undefined : { tail } })
    if (error || !data) throw new Error(`Could not read the preview gateway logs: ${getErrorMessage(error)}`)
    return data
  })
  if (options.json) {
    json(logs)
    return
  }
  if (logs.lines.length === 0) {
    info('The preview gateway has no log lines (is its container present? Run: temps preview-gateway status)')
    return
  }
  for (const line of logs.lines) console.log(line)
}

async function restartAction(options: ConfirmOptions): Promise<void> {
  await requireAuth()
  await setupClient()
  const confirmed = await confirmDisruption(
    ['Restarting the gateway interrupts every workspace preview URL until it is back.'],
    'Restart the preview gateway?',
    options,
  )
  if (!confirmed) return
  await withSpinner('Restarting preview gateway...', async () => {
    const { error } = await restartPreviewGateway({ client })
    if (error) throw new Error(`Could not restart the preview gateway: ${getErrorMessage(error)}`)
  })
  success('Preview gateway restarted')
  info('Check it with: temps preview-gateway status')
}

/** The upgrade request body: an explicit image, or `''` for the pinned default. */
export function buildUpgradeImage(options: UpgradeOptions): string {
  if (options.image !== undefined && options.default) {
    throw new ValidationError('Pass --image or --default, not both')
  }
  if (options.default) return ''
  const image = options.image?.trim()
  if (!image) {
    throw new ValidationError('Pass --image <ref> to choose the image, or --default for the one pinned in this Temps build')
  }
  return image
}

async function upgradeAction(options: UpgradeOptions): Promise<void> {
  const image = buildUpgradeImage(options)
  await requireAuth()
  await setupClient()
  const target = image || 'the image pinned in this Temps build'
  const confirmed = await confirmDisruption(
    [`Upgrading pulls ${target} and recreates the gateway; workspace preview URLs are unavailable meanwhile.`],
    'Upgrade the preview gateway?',
    options,
  )
  if (!confirmed) return
  await withSpinner(`Upgrading preview gateway to ${target}...`, async () => {
    const { error } = await upgradePreviewGateway({ client, body: { image } })
    if (error) throw new Error(`Could not upgrade the preview gateway: ${getErrorMessage(error)}`)
  })
  success(`Preview gateway upgraded to ${target}`)
  info('Check it with: temps preview-gateway status')
}
