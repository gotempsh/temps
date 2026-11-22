// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { VisitorInfo } from '@/api/client/types.gen'
import {
  countryFlag,
  regions,
} from '@/components/analytics/overview/AnalyticsDimensionIdentity-shared'
import { getReferrerDisplayName } from '@/components/analytics/overview/ReferrerIdentity'

/** The visitor list's filters a cell can set by clicking its value. */
export type IdentityFilterKey =
  | 'filter_country'
  | 'filter_region'
  | 'filter_city'
  | 'filter_channel'
  | 'filter_referrer'

export interface IdentityFilter {
  key: IdentityFilterKey
  value: string
}

function text(value: string | null | undefined): string | undefined {
  const trimmed = value?.trim()
  return trimmed ? trimmed : undefined
}

function countryFromCode(code: string | undefined): string | undefined {
  if (!code || !/^[a-z]{2}$/i.test(code)) return undefined
  const name = regions.of(code.toUpperCase())
  return name && name !== code.toUpperCase() ? name : undefined
}

export interface LocationSummary {
  flag?: string
  /** The parts shown on the line, in order, each with its filter if any. */
  parts: { label: string; filter?: IdentityFilter }[]
  /** Everything known, region included, for the tooltip. */
  detail?: string
}

export function locationSummary(
  visitor: Pick<VisitorInfo, 'city' | 'region' | 'country' | 'country_code'>
): LocationSummary {
  const city = text(visitor.city)
  const storedCountry = text(visitor.country)
  const code = text(visitor.country_code)
  const country = storedCountry ?? countryFromCode(code)
  const region = text(visitor.region)
  const distinctRegion =
    region && region !== city && region !== country ? region : undefined
  const parts: LocationSummary['parts'] = []
  if (city)
    parts.push({ label: city, filter: { key: 'filter_city', value: city } })
  if (country)
    parts.push({
      label: country,
      // The filter matches the stored name; a name derived from the code
      // might not, so it is shown without one.
      filter: storedCountry
        ? { key: 'filter_country', value: storedCountry }
        : undefined,
    })
  if (!city && !country && distinctRegion)
    parts.push({
      label: distinctRegion,
      filter: { key: 'filter_region', value: distinctRegion },
    })
  const known = [city, distinctRegion, country].filter(Boolean)
  return {
    flag: countryFlag(code ?? country ?? ''),
    parts,
    detail: known.length ? known.join(', ') : undefined,
  }
}

export interface SourceSummary {
  /** Referrer hostname, for its favicon; absent for direct or untagged visits. */
  domain?: string
  /** The channel whose icon stands in when there is no referrer hostname. */
  channel?: string
  primary: { label: string; filter?: IdentityFilter }
  secondary?: { label: string; filter: IdentityFilter }
  /** The full first referrer, or what is known, for the tooltip. */
  detail?: string
}

export function sourceSummary(
  visitor: Pick<
    VisitorInfo,
    'first_channel' | 'first_referrer' | 'first_referrer_hostname'
  >
): SourceSummary {
  const channel = text(visitor.first_channel)
  const domain = text(visitor.first_referrer_hostname)
  const referrer = text(visitor.first_referrer)
  const referrerPart = domain
    ? {
        label: getReferrerDisplayName(domain),
        filter: { key: 'filter_referrer', value: domain } as IdentityFilter,
      }
    : undefined
  const primary = channel
    ? {
        label: channel,
        filter: { key: 'filter_channel', value: channel } as IdentityFilter,
      }
    : (referrerPart ?? { label: 'Direct' })
  return {
    domain,
    channel,
    primary,
    secondary: channel ? referrerPart : undefined,
    detail: referrer ?? domain ?? channel,
  }
}

export interface TechnologySummary {
  browser?: string
  os?: string
  /** Versions, or the raw user agent when it named nothing. */
  detail?: string
}

function withVersion(name: string, version: string | undefined) {
  return version ? `${name} ${version}` : name
}

export function technologySummary(
  visitor: Pick<
    VisitorInfo,
    | 'browser'
    | 'browser_version'
    | 'operating_system'
    | 'operating_system_version'
    | 'user_agent'
  >
): TechnologySummary {
  const browser = text(visitor.browser)
  const os = text(visitor.operating_system)
  const browserDetail = browser
    ? withVersion(browser, text(visitor.browser_version))
    : undefined
  const osDetail = os
    ? withVersion(os, text(visitor.operating_system_version))
    : undefined
  const detail =
    browserDetail && osDetail
      ? `${browserDetail} on ${osDetail}`
      : (browserDetail ?? osDetail ?? text(visitor.user_agent))
  return { browser, os, detail }
}
