// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  getDeliveryProfile,
  listDeliveryProfiles,
  type DeliveryProfilePage,
  type DeliveryProfileResponse,
  type DeliveryProviderKind,
  type EnvironmentDeliveryOverride,
} from '@/api/client'
import { requireDeliveryData } from './delivery-errors'
import type { DeliveryProviderChoiceValue } from './DeliveryProviderChoice'

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

/**
 * Bunny counts as configured once a Bunny profile exists, so the delivery
 * capabilities live under the profiles root: creating or deleting a profile
 * invalidates the root and refreshes them with the lists.
 */
export const deliveryCapabilitiesQueryKey = [
  DELIVERY_PROFILES_QUERY_ROOT,
  'capabilities',
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

/** `Edge EU (Bunny)`: how a picker names a profile. */
export function deliveryProfileOptionLabel(
  profile: DeliveryProfileOption
): string {
  const provider =
    profile.provider_kind === 'direct'
      ? 'Direct'
      : profile.provider_kind === 'bunny'
        ? 'Bunny'
        : 'Cloudflare'
  return `${profile.name} (${provider})`
}

/** Query key of one profile, shared by its detail page and the pickers. */
export function deliveryProfileQueryKey(profileId: number) {
  return [DELIVERY_PROFILES_QUERY_ROOT, 'detail', profileId] as const
}

/**
 * One profile by ID; `null` when it does not exist, so callers can say so
 * instead of retrying a 404 as a failure.
 */
export async function fetchDeliveryProfile(
  profileId: number
): Promise<DeliveryProfileResponse | null> {
  const response = await getDeliveryProfile({
    path: { profile_id: profileId },
  })
  if (response.response?.status === 404) return null
  return requireDeliveryData(response)
}

/**
 * Profiles that environment overrides use but `listed` does not contain,
 * once each in ascending order: the ones a picker must load by ID to name
 * them and know their provider.
 */
export function unlistedOverrideProfileIds(
  overrides: EnvironmentDeliveryOverride[],
  listed: Pick<DeliveryProfileOption, 'id'>[]
): number[] {
  const ids = new Set<number>()
  for (const override of overrides) {
    const profileId = override.profile_id
    if (
      profileId != null &&
      !listed.some((profile) => profile.id === profileId)
    )
      ids.add(profileId)
  }
  return [...ids].sort((a, b) => a - b)
}

/** The overrides to save, or the profile whose provider is still unknown. */
export type ProviderChoiceOverrides =
  { overrides: EnvironmentDeliveryOverride[] } | { unknownProfileId: number }

/**
 * The environment overrides to save when the project default switches to
 * `selected`. Overrides only apply to projects with several environments,
 * so a single-environment project clears them. Choosing a CDN keeps every
 * override. Choosing no CDN clears overrides that pin a CDN profile and keeps
 * direct ones; an override whose profile kind is not known yet (`kindOf`
 * returns `undefined`) is reported instead of being cleared silently.
 */
export function overridesForProviderChoice(
  overrides: EnvironmentDeliveryOverride[],
  selected: DeliveryProviderChoiceValue,
  hasMultipleEnvironments: boolean,
  kindOf: (profileId: number) => DeliveryProviderKind | undefined
): ProviderChoiceOverrides {
  const result: EnvironmentDeliveryOverride[] = []
  for (const override of overrides) {
    const profileId = override.profile_id ?? null
    if (!hasMultipleEnvironments || profileId === null) {
      result.push({ ...override, profile_id: null })
      continue
    }
    if (selected !== 'none') {
      result.push(override)
      continue
    }
    const kind = kindOf(profileId)
    if (kind === undefined) return { unknownProfileId: profileId }
    result.push({
      ...override,
      profile_id: kind === 'direct' ? profileId : null,
    })
  }
  return { overrides: result }
}
