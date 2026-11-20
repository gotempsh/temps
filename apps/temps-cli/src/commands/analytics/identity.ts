// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps analytics identity ...`: how a project tells visitors apart
 * (ADR 109 D3). `persistent` keeps an id on the device; `cookieless` stores
 * nothing and counts visitors by a hash that changes every day.
 */

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { requireProjectSlug } from '../../config/resolve-project.js'
import {
  getAnalyticsIdentitySettings,
  getProjectBySlug,
  updateAnalyticsIdentitySettings,
} from '../../api/sdk.gen.js'
import type { AnalyticsIdentityMode, AnalyticsIdentitySettings } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { promptConfirm } from '../../ui/prompts.js'
import { newline, header, json, colors, success, info, warning } from '../../ui/output.js'

export const IDENTITY_MODES: AnalyticsIdentityMode[] = ['persistent', 'cookieless']

export function parseIdentityMode(raw: string): AnalyticsIdentityMode {
  const mode = raw.trim().toLowerCase() as AnalyticsIdentityMode
  if (!IDENTITY_MODES.includes(mode)) {
    throw new Error(`--mode must be one of ${IDENTITY_MODES.join(', ')}; got "${raw}"`)
  }
  return mode
}

/** Human-readable lines describing the settings, without colours. */
export function describeIdentity(settings: AnalyticsIdentitySettings): string[] {
  const lines =
    settings.mode === 'cookieless'
      ? ['Mode: cookieless (nothing stored on the visitor\'s device)']
      : ['Mode: persistent (visitor id kept in a first-party cookie or localStorage)']
  if (settings.sdk_option) {
    lines.push(`Tracker option: ${settings.sdk_option} (script tag: data-cookieless="true")`)
  }
  for (const limitation of settings.limitations) lines.push(`- ${limitation}`)
  return lines
}

interface IdentityOptions {
  project?: string
  json?: boolean
}

async function resolveProjectId(flagValue?: string): Promise<number> {
  const resolved = await requireProjectSlug(flagValue)
  if (resolved.source !== 'flag') {
    info(`Using project ${colors.bold(resolved.slug)} (from ${resolved.source})`)
  }
  const { data, error } = await getProjectBySlug({ client, path: { slug: resolved.slug } })
  if (error || !data) throw new Error(`Project "${resolved.slug}" not found`)
  return data.id
}

function print(settings: AnalyticsIdentitySettings): void {
  header('Visitor identity')
  for (const line of describeIdentity(settings)) info(line)
  newline()
}

async function showIdentity(options: IdentityOptions): Promise<void> {
  await requireAuth()
  await setupClient()
  const projectId = await resolveProjectId(options.project)
  const settings = await withSpinner('Fetching identity settings...', async () => {
    const { data, error } = await getAnalyticsIdentitySettings({
      client,
      path: { project_id: projectId },
    })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data
  })
  if (options.json) {
    json(settings)
    return
  }
  print(settings)
}

async function setIdentity(
  options: IdentityOptions & { mode: string; yes?: boolean },
): Promise<void> {
  const mode = parseIdentityMode(options.mode)
  await requireAuth()
  await setupClient()
  const projectId = await resolveProjectId(options.project)
  if (mode === 'cookieless' && !options.yes && !options.json) {
    warning('In cookieless mode a returning visitor counts as new the next day, and session replay is off.')
    const confirmed = await promptConfirm({ message: 'Switch to cookieless tracking?', default: false })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }
  const settings = await withSpinner('Saving identity mode...', async () => {
    const { data, error } = await updateAnalyticsIdentitySettings({
      client,
      path: { project_id: projectId },
      body: { mode },
    })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data
  })
  if (options.json) {
    json(settings)
    return
  }
  success(`Visitor identity is now ${settings.mode}`)
  print(settings)
}

export function registerAnalyticsIdentityCommands(analytics: Command): void {
  const identity = analytics
    .command('identity')
    .description('Whether visitors are recognised by a stored id or tracked cookieless')

  identity
    .command('show')
    .description('Show the identity mode and what it changes')
    .option('-p, --project <project>', 'Project slug or ID')
    .option('--json', 'Output in JSON format')
    .action(showIdentity)

  identity
    .command('set')
    .description('Switch between persistent and cookieless tracking')
    .option('-p, --project <project>', 'Project slug or ID')
    .requiredOption('--mode <mode>', `One of ${IDENTITY_MODES.join(', ')}`)
    .option('-y, --yes', 'Skip confirmation')
    .option('--json', 'Output in JSON format')
    .action(setIdentity)
}
