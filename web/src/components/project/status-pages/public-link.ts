// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * The part of a status page the public link is built from. Mirrors the API:
 * `public_url` is absent when the install has no public address, and
 * `public_url_status` says why and where to fix it.
 */
export interface PublicLinkSource {
  public_url?: string | null
  public_url_status?: {
    configured: boolean
    reason?: string | null
    setup_path?: string | null
  } | null
}

/** Where the operator sets the public address when the API does not say. */
export const DEFAULT_PUBLIC_URL_SETUP_PATH = '/settings#public-ingress'

export type PublicLinkState =
  /** A URL exists and nothing is known to be wrong with it. */
  | { kind: 'ready'; url: string }
  /** A URL exists, but will not open from outside this machine or network. */
  | { kind: 'warning'; url: string; reason: string; setupPath: string }
  /** No public address is configured: there is nothing to copy or open. */
  | { kind: 'missing'; reason: string; setupPath: string }

const MISSING_REASON =
  'Public URL is not configured. Set the external URL or the public ingress URL so published status pages get an address people outside your network can open.'

/**
 * Decide what the public link area shows. Never invents a URL: without
 * `public_url` the state is `missing`, whatever else the response says, so a
 * Copy or Open control can only ever be offered for a real address.
 */
export function describePublicLink(page: PublicLinkSource): PublicLinkState {
  const status = page.public_url_status
  const setupPath = status?.setup_path?.trim() || DEFAULT_PUBLIC_URL_SETUP_PATH
  const url = page.public_url?.trim()

  if (!url) {
    return {
      kind: 'missing',
      reason: status?.reason?.trim() || MISSING_REASON,
      setupPath,
    }
  }
  const reason = status?.reason?.trim()
  if (reason) {
    return { kind: 'warning', url, reason, setupPath }
  }
  return { kind: 'ready', url }
}

/** The URL to copy or open, or `null` when there is none. */
export function publicLinkUrl(page: PublicLinkSource): string | null {
  const state = describePublicLink(page)
  return state.kind === 'missing' ? null : state.url
}
