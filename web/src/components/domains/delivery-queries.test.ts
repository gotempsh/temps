// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { DeliveryProfileResponse } from '@/api/client'
import {
  DELIVERY_PROFILE_PICKER_QUERY,
  DELIVERY_PROFILES_QUERY_ROOT,
  deliveryPageCount,
  deliveryProfilePickerQueryKey,
  includeDeliveryProfile,
  isDeliveryProfileListTruncated,
  mayHaveDeliveryProfileOfKind,
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
