// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { QueryClient } from '@tanstack/react-query'
import type { DeliveryProfileResponse } from '@/api/client'
import {
  DELIVERY_PROFILE_PICKER_QUERY,
  DELIVERY_PROFILES_QUERY_ROOT,
  deliveryCapabilitiesQueryKey,
  deliveryPageCount,
  deliveryProfilePickerQueryKey,
  deliveryProfileOptionLabel,
  deliveryProfileQueryKey,
  includeDeliveryProfile,
  isDeliveryProfileListTruncated,
  mayHaveDeliveryProfileOfKind,
  overridesForProviderChoice,
  unlistedOverrideProfileIds,
} from './delivery-queries'

function profile(
  id: number,
  provider_kind: DeliveryProfileResponse['provider_kind'],
  name = `Profile ${id}`
): DeliveryProfileResponse {
  return {
    id,
    name,
    provider_kind,
    bunny_pull_zone_id: null,
    bunny_hostname: null,
    created_at: '2026-09-29T12:00:00Z',
    updated_at: '2026-09-29T12:00:00Z',
  }
}

describe('profile picker query', () => {
  test('asks for the first 100 profiles by name in one request', () => {
    expect(DELIVERY_PROFILE_PICKER_QUERY).toEqual({
      page: 1,
      page_size: 100,
      sort_by: 'name',
      sort_order: 'asc',
    })
  })

  test('is keyed by its parameters under the shared profile root', () => {
    expect(deliveryProfilePickerQueryKey[0]).toBe(DELIVERY_PROFILES_QUERY_ROOT)
    expect(deliveryProfilePickerQueryKey).toContain(
      DELIVERY_PROFILE_PICKER_QUERY
    )
  })
})

describe('isDeliveryProfileListTruncated', () => {
  test('a listing holding every profile is complete', () => {
    expect(
      isDeliveryProfileListTruncated({
        items: [profile(1, 'direct')],
        total: 1,
      })
    ).toBe(false)
    expect(isDeliveryProfileListTruncated({ items: [], total: 0 })).toBe(false)
  })

  test('a listing with fewer items than the total is partial', () => {
    expect(
      isDeliveryProfileListTruncated({
        items: [profile(1, 'direct')],
        total: 101,
      })
    ).toBe(true)
  })
})

describe('mayHaveDeliveryProfileOfKind', () => {
  test('nothing is known before the listing loads', () => {
    expect(mayHaveDeliveryProfileOfKind(undefined, 'bunny')).toBe(false)
  })

  test('a complete listing answers exactly', () => {
    const listing = { items: [profile(1, 'cloudflare')], total: 1 }
    expect(mayHaveDeliveryProfileOfKind(listing, 'cloudflare')).toBe(true)
    expect(mayHaveDeliveryProfileOfKind(listing, 'bunny')).toBe(false)
  })

  test('a partial listing never rules a kind out', () => {
    const listing = { items: [profile(1, 'cloudflare')], total: 250 }
    expect(mayHaveDeliveryProfileOfKind(listing, 'bunny')).toBe(true)
  })
})

describe('includeDeliveryProfile', () => {
  const listed = [profile(1, 'direct'), profile(2, 'cloudflare')]

  test('adds a selected profile the listing does not contain', () => {
    const selected = {
      id: 150,
      name: 'Zeta CDN',
      provider_kind: 'bunny' as const,
    }
    expect(includeDeliveryProfile(listed, selected)).toEqual([
      ...listed,
      selected,
    ])
  })

  test('never duplicates a profile that is already listed', () => {
    expect(includeDeliveryProfile(listed, profile(2, 'cloudflare'))).toBe(
      listed
    )
  })

  test('leaves the listing alone when nothing is selected', () => {
    expect(includeDeliveryProfile(listed, null)).toBe(listed)
    expect(includeDeliveryProfile(listed, undefined)).toBe(listed)
  })
})

describe('deliveryPageCount', () => {
  test('rounds a partial last page up', () => {
    expect(deliveryPageCount(41, 20)).toBe(3)
    expect(deliveryPageCount(40, 20)).toBe(2)
  })

  test('an empty list still has one page', () => {
    expect(deliveryPageCount(0, 20)).toBe(1)
  })
})

describe('profile labels and keys', () => {
  test('a picker names the profile and its provider', () => {
    expect(deliveryProfileOptionLabel(profile(1, 'bunny', 'Edge EU'))).toBe(
      'Edge EU (Bunny)'
    )
    expect(deliveryProfileOptionLabel(profile(2, 'direct', 'Origin'))).toBe(
      'Origin (Direct)'
    )
    expect(deliveryProfileOptionLabel(profile(3, 'cloudflare', 'CF'))).toBe(
      'CF (Cloudflare)'
    )
  })

  test('one profile shares the delivery-profiles key root', () => {
    expect(deliveryProfileQueryKey(7)).toEqual([
      DELIVERY_PROFILES_QUERY_ROOT,
      'detail',
      7,
    ])
  })
})

describe('unlistedOverrideProfileIds', () => {
  test('lists override profiles missing from the page, once each', () => {
    const overrides = [
      { environment_id: 1, profile_id: 9 },
      { environment_id: 2, profile_id: 3 },
      { environment_id: 3, profile_id: 9 },
      { environment_id: 4, profile_id: null },
      { environment_id: 5 },
    ]
    expect(
      unlistedOverrideProfileIds(overrides, [profile(3, 'direct')])
    ).toEqual([9])
  })
})

describe('overridesForProviderChoice', () => {
  const kinds = new Map([
    [1, 'direct'],
    [2, 'bunny'],
    [3, 'cloudflare'],
  ] as const)
  const kindOf = (profileId: number) => kinds.get(profileId as 1 | 2 | 3)
  const overrides = [
    { environment_id: 10, profile_id: 1 },
    { environment_id: 11, profile_id: 2 },
    { environment_id: 12, profile_id: 3 },
    { environment_id: 13, profile_id: null },
  ]

  test('choosing a CDN keeps every override', () => {
    expect(
      overridesForProviderChoice(overrides, 'bunny', true, kindOf)
    ).toEqual({ overrides })
  })

  test('choosing no CDN keeps direct overrides and clears CDN ones', () => {
    expect(overridesForProviderChoice(overrides, 'none', true, kindOf)).toEqual(
      {
        overrides: [
          { environment_id: 10, profile_id: 1 },
          { environment_id: 11, profile_id: null },
          { environment_id: 12, profile_id: null },
          { environment_id: 13, profile_id: null },
        ],
      }
    )
  })

  test('an override whose provider is unknown is reported, not cleared', () => {
    expect(
      overridesForProviderChoice(
        [...overrides, { environment_id: 14, profile_id: 99 }],
        'none',
        true,
        kindOf
      )
    ).toEqual({ unknownProfileId: 99 })
  })

  test('a single-environment project clears its overrides', () => {
    const result = overridesForProviderChoice(
      overrides,
      'cloudflare',
      false,
      kindOf
    )
    expect(result).toEqual({
      overrides: overrides.map((override) => ({
        ...override,
        profile_id: null,
      })),
    })
  })
})

describe('deliveryCapabilitiesQueryKey', () => {
  // Bunny becomes configured when its first profile is created, and the
  // profile mutations invalidate only the profiles root.
  test('is refreshed by invalidating the delivery profiles root', async () => {
    const client = new QueryClient()
    client.setQueryData(deliveryCapabilitiesQueryKey, [])
    await client.invalidateQueries({
      queryKey: [DELIVERY_PROFILES_QUERY_ROOT],
    })
    expect(
      client.getQueryState(deliveryCapabilitiesQueryKey)?.isInvalidated
    ).toBe(true)
  })
})
