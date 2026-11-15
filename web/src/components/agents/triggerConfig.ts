// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { TriggerConfig } from './AgentEditPage'

/** The trigger fields the workflow edit form lets the user change. */
export interface EditableTriggers {
  newIssue: boolean
  regression: boolean
  manual: boolean
  cron: string
}

/**
 * The `trigger_config` to save from the workflow edit form.
 *
 * For the triggers the form edits, only the blocks the user enabled are
 * written, so a save never persists a trigger nobody asked for -- except
 * `manual`, which is always written because the server reads its absence
 * as "on". Blocks the form does
 * not render -- the webhook, deploy and monitoring triggers -- are carried
 * over from `existing` unchanged. The server treats a missing `webhook` key
 * as "turned off" and deletes the webhook and its token, so dropping it here
 * would break every caller of an existing webhook on any unrelated edit.
 */
export function buildTriggerConfig(
  existing: TriggerConfig | null | undefined,
  edits: EditableTriggers
): TriggerConfig {
  const config: TriggerConfig = {}
  if (existing?.webhook !== undefined) config.webhook = existing.webhook
  if (existing?.deploy !== undefined) config.deploy = existing.deploy
  if (existing?.monitoring !== undefined) {
    config.monitoring = existing.monitoring
  }
  if (edits.newIssue || edits.regression) {
    config.error = { new_issue: edits.newIssue, regression: edits.regression }
  }
  // Always written: the server reads a missing `manual` as enabled, so
  // omitting it would turn the manual trigger on whenever it is off.
  config.manual = edits.manual
  const cron = edits.cron.trim()
  if (cron) config.schedule = { cron }
  return config
}
