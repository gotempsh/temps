// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { parseInstanceOrigin } from '@/lib/agent-handoff'

export interface HandoffOrigin {
  origin: string
  /**
   * `external_url`: the public address the operator set for this instance.
   * `browser`: the address this tab was loaded from.
   */
  source: 'external_url' | 'browser'
}

/**
 * The address a handoff prompt gives the agent, or `null` while it is not
 * known yet.
 *
 * The agent runs somewhere else than this tab: on the person's machine, or
 * on a remote one. The address bar is only how *this browser* reached the
 * console, which can be a port forward or a LAN name nothing else resolves.
 * The instance's `external_url` setting is the address the operator declared
 * public, and what the server itself puts in links it sends out, so it wins
 * when there is one.
 *
 * It is read from `GET /settings`, which needs `settings:read`. Someone
 * without it gets the address bar: for them the read fails, and that is an
 * answer, not a reason to keep waiting. Nothing is decided before the read
 * settles, so the prompt never shows one address and then another.
 */
export function resolveHandoffOrigin(input: {
  externalUrl: string | null | undefined
  /** The settings read finished, successfully or not. */
  settled: boolean
  browserOrigin: string
}): HandoffOrigin | null {
  if (!input.settled) return null
  const declared = parseInstanceOrigin(input.externalUrl)
  if (declared) return { origin: declared, source: 'external_url' }
  return { origin: input.browserOrigin, source: 'browser' }
}
