// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import {
  locationSummary,
  sourceSummary,
  technologySummary,
} from './visitor-identity'

const noLocation = {
  city: null,
  region: null,
  country: null,
  country_code: null,
}

test('a full location reads "City, Country" with its flag and keeps the region for the tooltip', () => {
  const summary = locationSummary({
    city: 'Lyon',
    region: 'Auvergne-Rhône-Alpes',
    country: 'France',
    country_code: 'FR',
  })
  expect(summary.flag).toBe('🇫🇷')
  expect(summary.parts).toEqual([
    { label: 'Lyon', filter: { key: 'filter_city', value: 'Lyon' } },
    { label: 'France', filter: { key: 'filter_country', value: 'France' } },
  ])
  expect(summary.detail).toBe('Lyon, Auvergne-Rhône-Alpes, France')
})

test('only a country shows its flag and name', () => {
  const summary = locationSummary({
    ...noLocation,
    country: 'Germany',
    country_code: 'DE',
  })
  expect(summary.flag).toBe('🇩🇪')
  expect(summary.parts.map((part) => part.label)).toEqual(['Germany'])
  expect(summary.detail).toBe('Germany')
})

test('a country known only by its code is named, but not offered as a filter', () => {
  const summary = locationSummary({ ...noLocation, country_code: 'JP' })
  expect(summary.flag).toBe('🇯🇵')
  expect(summary.parts).toEqual([{ label: 'Japan', filter: undefined }])
})

test('a region equal to the city is not repeated; a lone region is still shown', () => {
  expect(
    locationSummary({
      city: 'Madrid',
      region: 'Madrid',
      country: 'Spain',
      country_code: 'ES',
    }).detail
  ).toBe('Madrid, Spain')
  expect(locationSummary({ ...noLocation, region: 'Ontario' }).parts).toEqual([
    { label: 'Ontario', filter: { key: 'filter_region', value: 'Ontario' } },
  ])
})

test('nothing known, blank strings or a malformed code leave the location unknown', () => {
  for (const visitor of [
    noLocation,
    { city: ' ', region: '', country: '  ', country_code: '' },
    { ...noLocation, country_code: 'Z9' },
  ]) {
    const summary = locationSummary(visitor)
    expect(summary.parts).toEqual([])
    expect(summary.flag).toBeUndefined()
    expect(summary.detail).toBeUndefined()
  }
})

test('a referred visit leads with its channel, names the referrer and keeps the full URL', () => {
  const summary = sourceSummary({
    first_channel: 'Referral',
    first_referrer: 'https://blog.example.org/posts/launch-week',
    first_referrer_hostname: 'blog.example.org',
  })
  expect(summary.domain).toBe('blog.example.org')
  expect(summary.primary).toEqual({
    label: 'Referral',
    filter: { key: 'filter_channel', value: 'Referral' },
  })
  expect(summary.secondary).toEqual({
    label: 'blog.example.org',
    filter: { key: 'filter_referrer', value: 'blog.example.org' },
  })
  expect(summary.detail).toBe('https://blog.example.org/posts/launch-week')
})

test('a well-known referrer is shown by name and filters by its hostname', () => {
  const summary = sourceSummary({
    first_channel: null,
    first_referrer: null,
    first_referrer_hostname: 'www.google.com',
  })
  expect(summary.primary).toEqual({
    label: 'Google',
    filter: { key: 'filter_referrer', value: 'www.google.com' },
  })
  expect(summary.secondary).toBeUndefined()
  expect(summary.detail).toBe('www.google.com')
})

test('a channel without a referrer uses the channel alone; nothing at all reads Direct', () => {
  const email = sourceSummary({
    first_channel: 'Email',
    first_referrer: null,
    first_referrer_hostname: null,
  })
  expect(email.domain).toBeUndefined()
  expect(email.channel).toBe('Email')
  expect(email.secondary).toBeUndefined()
  const direct = sourceSummary({
    first_channel: null,
    first_referrer: null,
    first_referrer_hostname: null,
  })
  expect(direct.primary).toEqual({ label: 'Direct' })
  expect(direct.detail).toBeUndefined()
})

const noAgent = {
  browser: null,
  browser_version: null,
  operating_system: null,
  operating_system_version: null,
  user_agent: null,
}

test('browser and OS versions go to the tooltip', () => {
  expect(
    technologySummary({
      browser: 'Chrome',
      browser_version: '131.0.0.0',
      operating_system: 'Windows 10',
      operating_system_version: 'NT 10.0',
      user_agent: 'Mozilla/5.0',
    })
  ).toEqual({
    browser: 'Chrome',
    os: 'Windows 10',
    detail: 'Chrome 131.0.0.0 on Windows 10 NT 10.0',
  })
  expect(
    technologySummary({ ...noAgent, browser: 'Firefox', browser_version: null })
  ).toEqual({ browser: 'Firefox', os: undefined, detail: 'Firefox' })
})

test('an unrecognised user agent is kept for the tooltip; no user agent has no tooltip', () => {
  expect(
    technologySummary({ ...noAgent, user_agent: 'ExampleBot/1.0' })
  ).toEqual({
    browser: undefined,
    os: undefined,
    detail: 'ExampleBot/1.0',
  })
  expect(technologySummary(noAgent).detail).toBeUndefined()
})
