// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { FacetInfo } from '@/api/client/types.gen'

/** Slots available for facets across the whole platform. */
export const FACET_CAPACITY = 20

/** Mirrors the server's attribute-key charset: `[a-zA-Z][a-zA-Z0-9_.:-]*`. */
const FACET_KEY_PATTERN = /^[a-zA-Z][a-zA-Z0-9_.:-]*$/
const FACET_KEY_MAX_LENGTH = 200

/**
 * What an attribute filter means for the key a user typed.
 *
 * A facet is a dedicated, indexed column for one attribute key. Filtering on a
 * faceted key reads that column; filtering on any other key parses the JSON
 * attributes of every span in the time window, which is slow on large windows.
 */
export type AttributeKeyState =
  /** Nothing typed yet. */
  | { kind: 'empty' }
  /** The key cannot be sent as a filter as written. */
  | { kind: 'invalid'; reason: string }
  /** A completed facet: indexed and fully populated. */
  | { kind: 'ready'; facet: FacetInfo }
  /** A facet that is still indexing existing spans: fast, but incomplete. */
  | { kind: 'indexing'; facet: FacetInfo }
  /** A facet whose backfill failed. */
  | { kind: 'failed'; facet: FacetInfo }
  /** A facet being removed; it no longer filters fast. */
  | { kind: 'removing'; facet: FacetInfo }
  /** Not a facet: the filter would scan every span's attributes. */
  | { kind: 'unfaceted' }
  /** The facet list has not arrived, so the key cannot be classified yet. */
  | { kind: 'loading' }

/** The attribute filter pair is sent as `key=value`, split on `,` and `=`. */
export function validateAttributePair(
  key: string,
  value: string
): string | null {
  if (key.includes(',') || key.includes('=')) {
    return 'Attribute keys cannot contain "," or "=".'
  }
  if (value.includes(',')) {
    return 'Attribute values cannot contain ",": it separates filters.'
  }
  return null
}

export function classifyAttributeKey(
  facets: readonly FacetInfo[] | undefined,
  rawKey: string,
  value = ''
): AttributeKeyState {
  const key = rawKey.trim()
  if (!key) return { kind: 'empty' }
  const invalid = validateAttributePair(key, value)
  if (invalid) return { kind: 'invalid', reason: invalid }
  if (!facets) return { kind: 'loading' }
  const facet = facets.find((f) => f.attribute_key === key)
  if (!facet) return { kind: 'unfaceted' }
  switch (facet.status) {
    case 'completed':
      return { kind: 'ready', facet }
    case 'pending':
    case 'running':
      return { kind: 'indexing', facet }
    case 'failed':
      return { kind: 'failed', facet }
    case 'deleting':
      return { kind: 'removing', facet }
  }
}

/**
 * The `attributes` query value for the key/value a user entered, or undefined
 * when there is nothing valid to send. An empty value is not a filter.
 */
export function attributesQueryValue(
  rawKey: string,
  value: string
): string | undefined {
  const key = rawKey.trim()
  const trimmed = value.trim()
  if (!key || !trimmed) return undefined
  if (validateAttributePair(key, trimmed)) return undefined
  return `${key}=${trimmed}`
}

/** Why a facet cannot be created for `rawKey` right now, or null if it can. */
export function facetCreationBlocker(
  facets: readonly FacetInfo[],
  rawKey: string
): string | null {
  const key = rawKey.trim()
  if (!key) return 'Enter an attribute key first.'
  if (key.length > FACET_KEY_MAX_LENGTH) {
    return `Attribute keys are limited to ${FACET_KEY_MAX_LENGTH} characters.`
  }
  if (!FACET_KEY_PATTERN.test(key)) {
    return 'Facet keys start with a letter and use letters, digits and _ . : - only.'
  }
  if (facets.some((f) => f.attribute_key === key)) {
    return 'This key is already a facet.'
  }
  if (facets.length >= FACET_CAPACITY) {
    return `All ${FACET_CAPACITY} facet slots are in use. Remove one from a trace's attributes to add another.`
  }
  return null
}

/**
 * Whether the filter should be sent to the server for this key. With
 * `facetedOnly` an unfaceted key is never sent: the caller's endpoint would
 * scan every span of the project, so the user is offered the facet instead.
 */
export function attributeFilterApplies(
  state: AttributeKeyState,
  facetedOnly: boolean
): boolean {
  switch (state.kind) {
    case 'ready':
    case 'indexing':
    case 'failed':
      return true
    case 'unfaceted':
    case 'loading':
      // While the facet list loads a key could still turn out to be one, so a
      // caller that must never scan holds the filter back until it knows.
      return !facetedOnly
    case 'empty':
    case 'invalid':
    case 'removing':
      return false
  }
}
