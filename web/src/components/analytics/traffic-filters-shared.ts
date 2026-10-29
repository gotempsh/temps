// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Copy and helpers shared by the traffic-filter settings page and the inline
 * "Exclude from analytics" actions on reports.
 */

import type {
  FilteredTrafficReason,
  TrafficFilterKind,
} from '@/api/client/types.gen'

export const TRAFFIC_FILTER_KINDS: readonly TrafficFilterKind[] = [
  'referrer',
  'ip',
  'asn',
  'country',
]

interface KindCopy {
  label: string
  /** The thing being excluded, as a noun in a sentence ("Exclude this …?"). */
  noun: string
  /** Shown as the input placeholder: a concrete example of a valid value. */
  placeholder: string
  /** One line explaining how the value matches. */
  help: string
}

export const TRAFFIC_FILTER_KIND_COPY: Record<TrafficFilterKind, KindCopy> = {
  referrer: {
    label: 'Referrer',
    noun: 'referrer',
    placeholder: 'spam-site.example',
    help: 'Matches the domain and all of its subdomains. Pasting a full URL works.',
  },
  ip: {
    label: 'IP or range',
    noun: 'IP address',
    placeholder: '203.0.113.7 or 203.0.113.0/24',
    help: 'A single address, or a CIDR range up to /8 (IPv4) or /32 (IPv6).',
  },
  asn: {
    label: 'Network (ASN)',
    noun: 'network',
    placeholder: 'AS132203',
    help: 'Every IP announced by the network. Find the number on a visitor’s details.',
  },
  country: {
    label: 'Country',
    noun: 'country',
    placeholder: 'SG',
    help: 'A two-letter ISO country code.',
  },
}

export const FILTERED_TRAFFIC_REASON_LABELS: Record<
  FilteredTrafficReason,
  string
> = {
  bot: 'Bots & crawlers',
  datacenter: 'Datacenter & cloud',
  spam_referrer: 'Known referrer spam',
  referrer_rule: 'Your referrer filters',
  ip_rule: 'Your IP filters',
  asn_rule: 'Your network filters',
  country_rule: 'Your country filters',
}

/**
 * How a non-human visitor's `crawler_name` reads in the UI. Traffic excluded
 * by a filter is stored with a reason code (`datacenter`, `spam-referrer`,
 * `rule:<kind>`) in the same column a user-agent bot's name goes in; codes are
 * translated, bot names are shown as they are.
 */
export function crawlerLabel(
  crawlerName: string | null | undefined,
  fallback: string
): string {
  switch (crawlerName) {
    case null:
    case undefined:
    case '':
      return fallback
    case 'datacenter':
      return 'Datacenter traffic'
    case 'spam-referrer':
      return 'Referrer spam'
    case 'rule:referrer':
      return 'Excluded referrer'
    case 'rule:ip':
      return 'Excluded IP'
    case 'rule:asn':
      return 'Excluded network'
    case 'rule:country':
      return 'Excluded country'
    default:
      return crawlerName
  }
}

/** How a stored filter value reads in a list: networks get their `AS` prefix. */
export function formatTrafficFilterValue(
  kind: TrafficFilterKind,
  value: string
): string {
  return kind === 'asn' ? `AS${value}` : value
}

/**
 * Share of all events that were filtered out, as a whole-number percentage
 * string, or `null` when there were no events at all.
 */
export function filteredShare(
  countedEvents: number,
  filteredEvents: number
): string | null {
  const total = countedEvents + filteredEvents
  if (total <= 0) return null
  const share = (filteredEvents / total) * 100
  if (share > 0 && share < 1) return '<1%'
  return `${Math.round(share)}%`
}
