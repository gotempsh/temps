// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps cloud console-access` — ADR-045 §5's `cloud.console_access_enabled`
 * switch, the CLI parity for the console's "Console access through Temps
 * Cloud" toggle on the Temps Cloud settings page.
 *
 * `PATCH /cloud/features` is a small, dedicated endpoint that expects every
 * switch on every call (see `CloudFeatureSwitchesRequest` in
 * `crates/temps-cloud/src/handler.rs` -- deliberately not
 * `#[serde(default)]`), so `enable`/`disable` here always read the current
 * status first and send the other three switches back unchanged, exactly
 * like the console does.
 */

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { client, getErrorMessage, setupClient } from '../../lib/api-client.js'
import { getCloudStatus, updateCloudFeatures } from '../../api/sdk.gen.js'
import type { CloudStatus } from '../../api/types.gen.js'
import { header, icons, info, keyValue, newline } from '../../ui/output.js'
import { withSpinner } from '../../ui/spinner.js'

/**
 * Human-readable summary of the console-access switch for `status`.
 * Pure and exported so it can be unit tested without a live server --
 * three distinct states matter and each needs its own actionable text:
 * not linked at all, linked but off, and linked and on.
 */
export function describeConsoleAccess(status: CloudStatus): string {
  if (status.status !== 'linked') {
    return (
      'Not available — this instance is not connected to Temps Cloud. ' +
      'Run "temps cloud connect --code <code>" first.'
    )
  }
  if (!status.console_access_enabled) {
    return (
      'Off. Cloud members cannot open this console. ' +
      'Run "temps cloud console-access enable" to turn it on.'
    )
  }
  return (
    'On. Cloud members with the owner or admin role on this instance can ' +
    'open this console from Temps Cloud with no inbound port.'
  )
}

async function fetchStatus(): Promise<CloudStatus> {
  const { data, error } = await getCloudStatus({ client })
  if (error) throw new Error(getErrorMessage(error))
  if (!data) throw new Error('Temps Cloud status response was empty')
  return data
}

async function setConsoleAccess(enabled: boolean): Promise<CloudStatus> {
  await requireAuth()
  await setupClient()
  const current = await fetchStatus()
  const result = await withSpinner(
    enabled ? 'Enabling console access...' : 'Disabling console access...',
    async () => {
      const { data, error } = await updateCloudFeatures({
        client,
        body: {
          telemetry_enabled: current.telemetry_enabled,
          backups_enabled: current.backups_enabled,
          notifications_enabled: current.notifications_enabled,
          console_access_enabled: enabled,
        },
      })
      if (error) throw new Error(getErrorMessage(error))
      if (!data) throw new Error('Temps Cloud status response was empty')
      return data
    }
  )
  return result as CloudStatus
}

async function consoleAccessStatus(options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()
  const result = await withSpinner('Reading console access...', () => fetchStatus())
  if (!result) return
  if (options.json) {
    process.stdout.write(`${JSON.stringify(result, null, 2)}\n`)
    return
  }
  newline()
  header(`${icons.globe} Console access through Temps Cloud`)
  keyValue('Linked', result.status === 'linked' ? 'yes' : 'no')
  keyValue('Console access', result.console_access_enabled ? 'enabled' : 'disabled')
  info(describeConsoleAccess(result))
  newline()
}

async function consoleAccessEnable(options: { json?: boolean }): Promise<void> {
  const result = await setConsoleAccess(true)
  if (options.json) {
    process.stdout.write(`${JSON.stringify(result, null, 2)}\n`)
    return
  }
  info('Console access through Temps Cloud enabled.')
}

async function consoleAccessDisable(options: { json?: boolean }): Promise<void> {
  const result = await setConsoleAccess(false)
  if (options.json) {
    process.stdout.write(`${JSON.stringify(result, null, 2)}\n`)
    return
  }
  info(
    'Console access through Temps Cloud disabled. The Cloud sign-in provider ' +
      'and its sessions were revoked immediately.'
  )
}

export function registerCloudConsoleAccessCommands(cloud: Command): void {
  const consoleAccess = cloud
    .command('console-access')
    .description(
      'Console access through Temps Cloud (ADR-045) -- Cloud members with the owner/admin role can open this console with no inbound port'
    )

  consoleAccess
    .command('status')
    .description('Show whether Temps Cloud can open this console')
    .option('--json', 'Output JSON')
    .action(consoleAccessStatus)

  consoleAccess
    .command('enable')
    .description('Allow Temps Cloud to open this console')
    .option('--json', 'Output JSON')
    .action(consoleAccessEnable)

  consoleAccess
    .command('disable')
    .description(
      'Stop Temps Cloud from opening this console (revokes its sign-in provider and sessions immediately)'
    )
    .option('--json', 'Output JSON')
    .action(consoleAccessDisable)
}
