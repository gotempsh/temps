// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  listDeliveryProfiles,
  type DeliveryProfilePage,
  type DeliveryProfileResponse,
  type DeliveryProviderKind,
} from '@/api/client'
import { requireDeliveryData } from './delivery-errors'

/** What a profile picker needs to list and submit a profile. */
export type DeliveryProfileOption = Pick<
  DeliveryProfileResponse,
  'id' | 'name' | 'provider_kind'
>

/** The part of a list response that says how complete it is. */
export type DeliveryProfileListing = Pick<
  DeliveryProfilePage,
  'items' | 'total'
>

/**
 * Profile pickers load one page of the largest size the API serves, sorted
 * by name. Instances with more profiles say so next to the picker rather
 * than truncating silently (see `DeliveryProfileLimitNote`).
 */
export const DELIVERY_PROFILE_PICKER_QUERY = {
  page: 1,
  page_size: 100,
  sort_by: 'name',
  sort_order: 'asc',
} as const

/**
 * Every delivery-profile query key starts with this, so invalidating it
 * refreshes the list pages, the pickers and the detail page together.
 */
export const DELIVERY_PROFILES_QUERY_ROOT = 'delivery-profiles'

export const deliveryProfilePickerQueryKey = [
  DELIVERY_PROFILES_QUERY_ROOT,
  'list',
  DELIVERY_PROFILE_PICKER_QUERY,
] as const

export async function fetchDeliveryProfilePicker(): Promise<DeliveryProfilePage> {
  return requireDeliveryData(
    await listDeliveryProfiles({ query: DELIVERY_PROFILE_PICKER_QUERY })
  )
}

/** Whether more profiles exist than `listing` contains. */
export function isDeliveryProfileListTruncated(
  listing: DeliveryProfileListing
): boolean {
  return listing.total > listing.items.length
}

/**
 * Whether a profile of `kind` may exist. Exact when `listing` holds every
 * profile. When it is partial, a kind it does not contain is unknown rather
 * than absent, so this answers true and leaves the decision to the server,
 * which validates the choice and explains a refusal.
 */
export function mayHaveDeliveryProfileOfKind(
  listing: DeliveryProfileListing | undefined,
  kind: DeliveryProviderKind
): boolean {
  if (!listing) return false
  return (
    listing.items.some((profile) => profile.provider_kind === kind) ||
    isDeliveryProfileListTruncated(listing)
  )
}

/**
 * `profiles` plus `extra` when it is missing, so a profile that is already
 * selected stays visible in a picker that only lists the first page.
 */
export function includeDeliveryProfile(
  profiles: DeliveryProfileOption[],
  extra: DeliveryProfileOption | null | undefined
): DeliveryProfileOption[] {
  if (!extra || profiles.some((profile) => profile.id === extra.id))
    return profiles
  return [...profiles, extra]
}

/** Number of pages `total` rows fill; an empty list still has page 1. */
export function deliveryPageCount(total: number, pageSize: number): number {
  return Math.max(1, Math.ceil(total / pageSize))
}
