// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps analytics identity ...`: how a project tells visitors apart
 * (ADR 109 D3). `persistent` keeps an id on the device; `cookieless` stores
 * nothing and counts visitors by a hash that changes every day. Also whether
 * `identify()` from the browser is accepted (ADR 109 D4).
 */

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { resolveAnalyticsProject } from './project.js'
import {
  getAnalyticsIdentitySettings,
  updateAnalyticsIdentitySettings,
} from '../../api/sdk.gen.js'
import type { AnalyticsIdentityMode, AnalyticsIdentitySettings } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { promptConfirm } from '../../ui/prompts.js'
import { newline, header, json, colors, success, info, warning } from '../../ui/output.js'

export const IDENTITY_MODES: AnalyticsIdentityMode[] = ['persistent', 'cookieless']

/** `--browser-identify accept|refuse` to the API's boolean. */
export function parseBrowserIdentify(raw: string): boolean {
  const value = raw.trim().toLowerCase()
  if (value === 'accept') return true
  if (value === 'refuse') return false
  throw new Error(`--browser-identify must be accept or refuse; got "${raw}"`)
}

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
  if (settings.mode === 'cookieless') {
    lines.push('Identify: stores nothing in cookieless mode, from the browser or the backend')
  } else if (settings.accept_browser_identify) {
    lines.push('Browser identify: accepted (identify() from the page labels its own visitor)')
  } else {
    lines.push('Browser identify: refused (only the app\'s backend can identify a visitor)')
  }
  for (const limitation of settings.limitations) lines.push(`- ${limitation}`)
  return lines
}

interface IdentityOptions {
  project?: string
  json?: boolean
}

function print(settings: AnalyticsIdentitySettings): void {
  header('Visitor identity')
  for (const line of describeIdentity(settings)) info(line)
  newline()
}

async function showIdentity(options: IdentityOptions): Promise<void> {
  await requireAuth()
  await setupClient()
  const projectId = (await resolveAnalyticsProject(options.project)).id
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
  options: IdentityOptions & { mode?: string; browserIdentify?: string; yes?: boolean },
): Promise<void> {
  if (options.mode === undefined && options.browserIdentify === undefined) {
    throw new Error('Pass --mode, --browser-identify, or both')
  }
  const mode = options.mode === undefined ? undefined : parseIdentityMode(options.mode)
  const acceptBrowserIdentify =
    options.browserIdentify === undefined ? undefined : parseBrowserIdentify(options.browserIdentify)
  await requireAuth()
  await setupClient()
  const projectId = (await resolveAnalyticsProject(options.project)).id
  if (mode === 'cookieless' && !options.yes && !options.json) {
    warning('In cookieless mode a returning visitor counts as new the next day, and session replay is off.')
    const confirmed = await promptConfirm({ message: 'Switch to cookieless tracking?', default: false })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }
  const settings = await withSpinner('Saving identity settings...', async () => {
    const { data, error } = await updateAnalyticsIdentitySettings({
      client,
      path: { project_id: projectId },
      body: { mode, accept_browser_identify: acceptBrowserIdentify },
    })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data
  })
  if (options.json) {
    json(settings)
    return
  }
  success(`Visitor identity is now ${settings.mode}`)
  if (acceptBrowserIdentify !== undefined) {
    success(`Browser identify is now ${settings.accept_browser_identify ? 'accepted' : 'refused'}`)
  }
  print(settings)
}

export function registerAnalyticsIdentityCommands(analytics: Command): void {
  const identity = analytics
    .command('identity')
    .description(
      'Whether visitors are recognised by a stored id or tracked cookieless, and who may identify them',
    )

  identity
    .command('show')
    .description('Show the identity mode, whether browser identify is accepted, and what they change')
    .option('-p, --project <project>', 'Project slug or ID')
    .option('--json', 'Output in JSON format')
    .action(showIdentity)

  identity
    .command('set')
    .description('Switch between persistent and cookieless tracking, or accept/refuse browser identify')
    .option('-p, --project <project>', 'Project slug or ID')
    .option('--mode <mode>', `One of ${IDENTITY_MODES.join(', ')}`)
    .option(
      '--browser-identify <accept|refuse>',
      'Whether identify() from the browser is applied; refuse leaves identify to the app\'s backend',
    )
    .option('-y, --yes', 'Skip confirmation')
    .option('--json', 'Output in JSON format')
    .action(setIdentity)
}
