// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'

import {
  crawlerLabel,
  FILTERED_TRAFFIC_REASON_LABELS,
  TRAFFIC_FILTER_KINDS,
  TRAFFIC_FILTER_KIND_COPY,
  filteredShare,
  formatTrafficFilterValue,
} from './traffic-filters-shared'

describe('formatTrafficFilterValue', () => {
  it('prefixes network numbers with AS', () => {
    expect(formatTrafficFilterValue('asn', '132203')).toBe('AS132203')
  })

  it('leaves other kinds unchanged', () => {
    expect(formatTrafficFilterValue('referrer', 'spam-site.example')).toBe(
      'spam-site.example'
    )
    expect(formatTrafficFilterValue('ip', '203.0.113.0/24')).toBe(
      '203.0.113.0/24'
    )
    expect(formatTrafficFilterValue('country', 'SG')).toBe('SG')
  })
})

describe('filteredShare', () => {
  it('is null when nothing was recorded', () => {
    expect(filteredShare(0, 0)).toBeNull()
  })

  it('rounds to a whole percentage of all events', () => {
    expect(filteredShare(900, 100)).toBe('10%')
    expect(filteredShare(0, 50)).toBe('100%')
    expect(filteredShare(50, 0)).toBe('0%')
  })

  it('never rounds a non-zero share down to 0%', () => {
    expect(filteredShare(10_000, 1)).toBe('<1%')
  })
})

describe('copy tables', () => {
  it('describe every filter kind', () => {
    for (const kind of TRAFFIC_FILTER_KINDS) {
      expect(TRAFFIC_FILTER_KIND_COPY[kind].label.length).toBeGreaterThan(0)
      expect(TRAFFIC_FILTER_KIND_COPY[kind].placeholder.length).toBeGreaterThan(
        0
      )
    }
  })

  it('label every filtered-traffic reason', () => {
    expect(Object.keys(FILTERED_TRAFFIC_REASON_LABELS).sort()).toEqual(
      [
        'asn_rule',
        'bot',
        'country_rule',
        'datacenter',
        'ip_rule',
        'referrer_rule',
        'spam_referrer',
      ].sort()
    )
  })
})

describe('crawlerLabel', () => {
  it('translates exclusion reason codes', () => {
    expect(crawlerLabel('datacenter', 'Bot')).toBe('Datacenter traffic')
    expect(crawlerLabel('spam-referrer', 'Bot')).toBe('Referrer spam')
    expect(crawlerLabel('rule:country', 'Bot')).toBe('Excluded country')
  })

  it('shows bot names as they are, and falls back when there is none', () => {
    expect(crawlerLabel('Googlebot', 'Bot')).toBe('Googlebot')
    expect(crawlerLabel(null, 'Crawler')).toBe('Crawler')
    expect(crawlerLabel('', 'Bot')).toBe('Bot')
  })
})
