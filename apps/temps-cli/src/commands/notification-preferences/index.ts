// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  getPreferences,
  updatePreferences,
  deletePreferences,
} from '../../api/sdk.gen.js'
import type { NotificationPreferencesResponse } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { promptConfirm } from '../../ui/prompts.js'
import { newline, header, icons, json, colors, success, info, warning, keyValue } from '../../ui/output.js'

interface ShowOptions {
  json?: boolean
}

interface UpdateOptions {
  key: string
  value: string
}

interface ResetOptions {
  force?: boolean
  yes?: boolean
}

const BOOLEAN_KEYS: Array<keyof NotificationPreferencesResponse> = [
  'email_enabled',
  'slack_enabled',
  'weekly_digest_enabled',
  'batch_similar_notifications',
  'deployment_failures_enabled',
  'build_errors_enabled',
  'runtime_errors_enabled',
  'ssl_expiration_enabled',
  'domain_expiration_enabled',
  'dns_changes_enabled',
  'backup_failures_enabled',
  'backup_successes_enabled',
  'route_downtime_enabled',
  'load_balancer_issues_enabled',
  's3_connection_issues_enabled',
  'retention_policy_violations_enabled',
]

const NUMBER_KEYS: Array<keyof NotificationPreferencesResponse> = [
  'error_threshold',
  'error_time_window',
  'ssl_days_before_expiration',
]

const STRING_KEYS: Array<keyof NotificationPreferencesResponse> = [
  'minimum_severity',
  'digest_send_time',
  'digest_send_day',
]

const ALL_KEYS = [...BOOLEAN_KEYS, ...NUMBER_KEYS, ...STRING_KEYS]

export function registerNotificationPreferencesCommands(program: Command): void {
  const prefs = program
    .command('notification-preferences')
    .alias('notif-prefs')
    .description('Manage notification preferences')

  prefs
    .command('show')
    .alias('get')
    .description('Show current notification preferences')
    .option('--json', 'Output in JSON format')
    .action(showPreferencesAction)

  prefs
    .command('update')
    .alias('set')
    .description('Update a notification preference')
    .requiredOption('-k, --key <key>', 'Preference key to update')
    .requiredOption('-v, --value <value>', 'Value for the preference')
    .action(updatePreferencesAction)

  prefs
    .command('reset')
    .description('Reset notification preferences to defaults')
    .option('-f, --force', 'Skip confirmation')
    .option('-y, --yes', 'Skip confirmation prompts (alias for --force)')
    .action(resetPreferencesAction)
}

export function formatBooleanValue(value: boolean): string {
  return value ? colors.success('enabled') : colors.muted('disabled')
}

export function isKnownPreferenceKey(key: string): key is keyof NotificationPreferencesResponse {
  return ALL_KEYS.includes(key as keyof NotificationPreferencesResponse)
}

export function parsePreferenceValue(
  key: keyof NotificationPreferencesResponse,
  value: string,
): boolean | number | string {
  if (BOOLEAN_KEYS.includes(key)) {
    if (value !== 'true' && value !== 'false') {
      throw new Error(`Invalid value for "${key}". Expected "true" or "false"`)
    }
    return value === 'true'
  }

  if (NUMBER_KEYS.includes(key)) {
    const parsed = parseInt(value, 10)
    if (isNaN(parsed)) {
      throw new Error(`Invalid value for "${key}". Expected a number`)
    }
    return parsed
  }

  return value
}

async function showPreferencesAction(options: ShowOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const prefs = await withSpinner('Fetching notification preferences...', async () => {
    const { data, error } = await getPreferences({ client })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  if (!prefs) {
    warning('Notification preferences not found')
    return
  }

  if (options.json) {
    json(prefs)
    return
  }

  newline()
  header(`${icons.info} Notification Preferences`)

  // Channels
  newline()
  header('Channels')
  keyValue('Email', formatBooleanValue(prefs.email_enabled))
  keyValue('Slack', formatBooleanValue(prefs.slack_enabled))

  // Event Types
  newline()
  header('Event Notifications')
  keyValue('Deployment Failures', formatBooleanValue(prefs.deployment_failures_enabled))
  keyValue('Build Errors', formatBooleanValue(prefs.build_errors_enabled))
  keyValue('Runtime Errors', formatBooleanValue(prefs.runtime_errors_enabled))
  keyValue('SSL Expiration', formatBooleanValue(prefs.ssl_expiration_enabled))
  keyValue('Domain Expiration', formatBooleanValue(prefs.domain_expiration_enabled))
  keyValue('DNS Changes', formatBooleanValue(prefs.dns_changes_enabled))
  keyValue('Backup Failures', formatBooleanValue(prefs.backup_failures_enabled))
  keyValue('Backup Successes', formatBooleanValue(prefs.backup_successes_enabled))
  keyValue('Route Downtime', formatBooleanValue(prefs.route_downtime_enabled))
  keyValue('Load Balancer Issues', formatBooleanValue(prefs.load_balancer_issues_enabled))
  keyValue('S3 Connection Issues', formatBooleanValue(prefs.s3_connection_issues_enabled))
  keyValue('Retention Policy Violations', formatBooleanValue(prefs.retention_policy_violations_enabled))

  // Thresholds
  newline()
  header('Thresholds')
  keyValue('Error Threshold', prefs.error_threshold)
  keyValue('Error Time Window', `${prefs.error_time_window}s`)
  keyValue('SSL Days Before Expiration', `${prefs.ssl_days_before_expiration} days`)
  keyValue('Minimum Severity', prefs.minimum_severity)

  // Digest
  newline()
  header('Weekly Digest')
  keyValue('Enabled', formatBooleanValue(prefs.weekly_digest_enabled))
  keyValue('Send Day', prefs.digest_send_day)
  keyValue('Send Time', prefs.digest_send_time)
  keyValue('Batch Similar', formatBooleanValue(prefs.batch_similar_notifications))

  if (prefs.digest_sections) {
    newline()
    header('Digest Sections')
    if (prefs.digest_sections.deployments !== undefined) {
      keyValue('Deployments', formatBooleanValue(prefs.digest_sections.deployments))
    }
    if (prefs.digest_sections.errors !== undefined) {
      keyValue('Errors', formatBooleanValue(prefs.digest_sections.errors))
    }
    if (prefs.digest_sections.funnels !== undefined) {
      keyValue('Funnels', formatBooleanValue(prefs.digest_sections.funnels))
    }
    if (prefs.digest_sections.performance !== undefined) {
      keyValue('Performance', formatBooleanValue(prefs.digest_sections.performance))
    }
    if (prefs.digest_sections.projects !== undefined) {
      keyValue('Projects', formatBooleanValue(prefs.digest_sections.projects))
    }
  }

  newline()
}

async function updatePreferencesAction(options: UpdateOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const { key, value } = options

  // Validate the key
  if (!isKnownPreferenceKey(key)) {
    warning(`Unknown preference key: ${key}`)
    info(`Available keys: ${ALL_KEYS.join(', ')}`)
    return
  }

  // Fetch current preferences to merge with update
  const currentPrefs = await withSpinner('Fetching current preferences...', async () => {
    const { data, error } = await getPreferences({ client })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  if (!currentPrefs) {
    warning('Could not fetch current preferences')
    return
  }

  // Parse the value based on key type
  let parsedValue: boolean | number | string
  try {
    parsedValue = parsePreferenceValue(key, value)
  } catch (e) {
    warning((e as Error).message)
    return
  }

  // Build the updated preferences object
  const updatedPrefs = { ...currentPrefs, [key]: parsedValue }

  await withSpinner('Updating preferences...', async () => {
    const { error } = await updatePreferences({
      client,
      body: {
        preferences: updatedPrefs,
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success(`Preference "${key}" updated to "${value}"`)
}

async function resetPreferencesAction(options: ResetOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const skipConfirmation = options.force || options.yes

  if (!skipConfirmation) {
    const confirmed = await promptConfirm({
      message: 'Reset all notification preferences to defaults? This cannot be undone.',
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  await withSpinner('Resetting notification preferences...', async () => {
    const { error } = await deletePreferences({ client })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success('Notification preferences reset to defaults')
}
