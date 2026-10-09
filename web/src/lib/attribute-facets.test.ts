// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { FacetInfo, FacetStatus } from '@/api/client/types.gen'
import {
  FACET_CAPACITY,
  attributeFilterApplies,
  attributesQueryValue,
  classifyAttributeKey,
  facetCreationBlocker,
} from './attribute-facets'

function facet(key: string, status: FacetStatus = 'completed'): FacetInfo {
  return {
    attribute_key: key,
    backend: 'clickhouse',
    created_at: '2026-01-01T00:00:00Z',
    rows_backfilled: 0,
    slot: 1,
    status,
  }
}

describe('classifyAttributeKey', () => {
  const facets = [
    facet('tier'),
    facet('region', 'running'),
    facet('plan', 'pending'),
    facet('broken', 'failed'),
    facet('old', 'deleting'),
  ]

  test('an empty or blank key is not a filter', () => {
    expect(classifyAttributeKey(facets, '').kind).toBe('empty')
    expect(classifyAttributeKey(facets, '   ').kind).toBe('empty')
  })

  test('a completed facet is ready; one still backfilling is indexing', () => {
    expect(classifyAttributeKey(facets, 'tier').kind).toBe('ready')
    expect(classifyAttributeKey(facets, 'region').kind).toBe('indexing')
    expect(classifyAttributeKey(facets, 'plan').kind).toBe('indexing')
  })

  test('failed and removing facets are reported as such, not as fast', () => {
    expect(classifyAttributeKey(facets, 'broken').kind).toBe('failed')
    expect(classifyAttributeKey(facets, 'old').kind).toBe('removing')
  })

  test('any other key is unfaceted, including with no facets at all', () => {
    expect(classifyAttributeKey(facets, 'http.route').kind).toBe('unfaceted')
    expect(classifyAttributeKey([], 'tier').kind).toBe('unfaceted')
  })

  test('a key cannot be classified before the facet list arrives', () => {
    expect(classifyAttributeKey(undefined, 'tier').kind).toBe('loading')
    expect(classifyAttributeKey(undefined, '').kind).toBe('empty')
  })

  test('surrounding whitespace does not hide a facet', () => {
    expect(classifyAttributeKey(facets, '  tier ').kind).toBe('ready')
  })

  test('a key or value the server would split is invalid, not sent wrong', () => {
    expect(classifyAttributeKey(facets, 'a,b').kind).toBe('invalid')
    expect(classifyAttributeKey(facets, 'a=b').kind).toBe('invalid')
    expect(classifyAttributeKey(facets, 'tier', 'x,y').kind).toBe('invalid')
    expect(classifyAttributeKey(facets, 'tier', 'x=y').kind).toBe('ready')
  })
})

describe('attributesQueryValue', () => {
  test('sends key=value only when both halves are present and valid', () => {
    expect(attributesQueryValue('tier', 'free')).toBe('tier=free')
    expect(attributesQueryValue(' tier ', ' free ')).toBe('tier=free')
    expect(attributesQueryValue('tier', '')).toBeUndefined()
    expect(attributesQueryValue('', 'free')).toBeUndefined()
    expect(attributesQueryValue('tier', 'a,b')).toBeUndefined()
    expect(attributesQueryValue('a=b', 'c')).toBeUndefined()
  })

  test('a value may contain an equals sign', () => {
    expect(attributesQueryValue('query', 'a=b')).toBe('query=a=b')
  })
})

describe('facetCreationBlocker', () => {
  test('allows a fresh, well-formed key', () => {
    expect(facetCreationBlocker([], 'http.route')).toBeNull()
    expect(facetCreationBlocker([facet('tier')], 'enduser.id')).toBeNull()
  })

  test('explains why a key cannot be created', () => {
    expect(facetCreationBlocker([], '')).toContain('Enter')
    expect(facetCreationBlocker([], '1abc')).toContain('start with a letter')
    expect(facetCreationBlocker([], 'a b')).toContain('letters, digits')
    expect(facetCreationBlocker([], 'a'.repeat(201))).toContain('200')
    expect(facetCreationBlocker([facet('tier')], 'tier')).toContain(
      'already a facet'
    )
  })

  test('names the capacity limit when every slot is taken', () => {
    const full = Array.from({ length: FACET_CAPACITY }, (_, i) =>
      facet(`key${i}`)
    )
    expect(facetCreationBlocker(full, 'another')).toContain(
      `All ${FACET_CAPACITY} facet slots`
    )
  })
})

describe('attributeFilterApplies', () => {
  const key = (value: string) => classifyAttributeKey([facet('tier')], value)

  test('facets always filter, whatever endpoint is asking', () => {
    expect(attributeFilterApplies(key('tier'), true)).toBe(true)
    expect(attributeFilterApplies(key('tier'), false)).toBe(true)
  })

  test('an unfaceted key filters only where scanning is acceptable', () => {
    expect(attributeFilterApplies(key('other'), false)).toBe(true)
    expect(attributeFilterApplies(key('other'), true)).toBe(false)
  })

  test('a faceted-only caller holds back while the list loads', () => {
    const loading = classifyAttributeKey(undefined, 'tier')
    expect(attributeFilterApplies(loading, true)).toBe(false)
    expect(attributeFilterApplies(loading, false)).toBe(true)
  })

  test('nothing is sent for an empty or invalid key', () => {
    expect(attributeFilterApplies(key(''), false)).toBe(false)
    expect(attributeFilterApplies(key('a,b'), false)).toBe(false)
  })
})
