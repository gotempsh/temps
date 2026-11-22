// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps analytics identify <visitor-id> --user-id <id>` (ADR 109 D4): label
 * a visitor with the app's user id, as the app's backend would. The identity
 * is marked as set by the server, so `identify()` from the browser cannot
 * replace it.
 */

import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { resolveAnalyticsProject } from './project.js'
import { identifyVisitor } from '../../api/sdk.gen.js'
import type { IdentifyVisitorResponse } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { newline, json as jsonOut, success, keyValue } from '../../ui/output.js'
import { buildEnrichPayload } from './enrich.js'

export interface IdentifyCommandOptions {
  project?: string
  userId?: string
  data?: string
  set?: string[]
  unset?: string[]
  json?: boolean
}

export type IdentifyBody = { user_id: string; traits?: Record<string, unknown> }

/** The request body, or the reason the flags cannot make one. */
export function buildIdentifyBody(
  options: IdentifyCommandOptions,
): { ok: true; body: IdentifyBody } | { ok: false; error: string } {
  const userId = options.userId?.trim() ?? ''
  if (userId === '') {
    return { ok: false, error: 'Pass --user-id with your app\'s id for this person, e.g. --user-id user_123.' }
  }
  const wantsTraits =
    options.data !== undefined || (options.set?.length ?? 0) > 0 || (options.unset?.length ?? 0) > 0
  if (!wantsTraits) return { ok: true, body: { user_id: userId } }
  const built = buildEnrichPayload({ data: options.data, set: options.set, unset: options.unset })
  if (!built.ok) return { ok: false, error: built.error.replace('Nothing to enrich', 'No traits') }
  return { ok: true, body: { user_id: userId, traits: built.payload } }
}

/** Why nothing was stored, worded for the terminal, or `null` when it was. */
export function identifyFailure(visitorId: string, response: IdentifyVisitorResponse): string | null {
  if (response.applied) return null
  return `Visitor "${visitorId}" was not identified (${response.outcome}): ${response.message}`
}

export async function identifyVisitorAction(
  visitorIdArg: string,
  options: IdentifyCommandOptions,
): Promise<void> {
  const visitorId = visitorIdArg.trim()
  if (visitorId === '') {
    throw new Error(
      'Visitor ID is required: the visitor GUID, or the sealed enc_... ID from the _temps_visitor_id cookie.',
    )
  }
  const built = buildIdentifyBody(options)
  if (!built.ok) throw new Error(built.error)

  await requireAuth()
  await setupClient()
  const project = await resolveAnalyticsProject(options.project)

  const response = await withSpinner('Identifying visitor...', async () => {
    const { data, error } = await identifyVisitor({
      client,
      path: { project_id: project.id, visitor_id: visitorId },
      body: built.body,
    })
    if (error) throw new Error(getErrorMessage(error))
    if (!data) {
      throw new Error(
        `Identify request for visitor "${visitorId}" returned no response body. Re-run with DEBUG=1, and check the Temps instance is reachable.`,
      )
    }
    return data
  })

  if (options.json) jsonOut(response)
  const failure = identifyFailure(visitorIdArg, response)
  if (failure) throw new Error(failure)
  if (options.json) return

  newline()
  success(response.message)
  keyValue('Visitor', visitorIdArg)
  keyValue('User id', built.body.user_id)
  if (built.body.traits) keyValue('Traits', Object.keys(built.body.traits).join(', '))
  newline()
}
