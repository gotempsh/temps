// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ProviderKeyResponse } from '@/api/client'

/**
 * What the AI Gateway page may claim about one supported provider.
 *
 * `unknown` exists because the provider-key list can fail to load (5xx,
 * network error, 403). An absent list is not an empty one: saying "Not
 * configured" then tells an operator with working keys that they have none.
 */
export type AiProviderRowStatus =
  'active' | 'disabled' | 'not-configured' | 'unknown'

export interface ProviderKeysReadState {
  /** Last successfully read keys; `undefined` when no read has succeeded. */
  keys: ProviderKeyResponse[] | undefined
  /** Whether the most recent read of the key list failed. */
  isError: boolean
}

/**
 * True when the latest read failed and no key is cached to show instead.
 *
 * An empty cache counts as unknown too: the failed refresh is the current
 * answer, and the last good read being empty does not prove it still is.
 */
export function providerKeysUnknown({
  keys,
  isError,
}: ProviderKeysReadState): boolean {
  return isError && !keys?.length
}

export function aiProviderRowStatus(
  providerId: string,
  read: ProviderKeysReadState
): AiProviderRowStatus {
  if (providerKeysUnknown(read)) return 'unknown'
  const providerKeys = (read.keys ?? []).filter(
    (key) => key.provider === providerId
  )
  if (providerKeys.some((key) => key.is_active)) return 'active'
  if (providerKeys.length > 0) return 'disabled'
  // "Not configured" is a claim only a successful read can make. A cached key
  // keeps its last-known status under the stale banner; a provider with none
  // cached is unknown until the list reads again.
  return read.isError ? 'unknown' : 'not-configured'
}

/**
 * The hero's "Add a provider key" call to action is onboarding for an
 * instance with no active key. It may only appear once a read has proven
 * that, never when the read failed.
 */
export function shouldPromptForFirstProviderKey(
  read: ProviderKeysReadState,
  supportedProviderIds: readonly string[]
): boolean {
  if (read.isError || read.keys === undefined) return false
  return !read.keys.some(
    (key) => key.is_active && supportedProviderIds.includes(key.provider)
  )
}
