// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import { analyticsIngestBaseUrl } from './analytics-ingest-url'

const origin = 'http://localhost:3200'

describe('analyticsIngestBaseUrl', () => {
  it('uses the configured external URL, not the address bar', () => {
    expect(
      analyticsIngestBaseUrl({
        externalUrl: 'https://temps.example.test',
        isLoaded: true,
        origin,
      })
    ).toEqual({
      url: 'https://temps.example.test/api/_temps',
      source: 'external_url',
    })
  })

  it('does not double a trailing slash', () => {
    expect(
      analyticsIngestBaseUrl({
        externalUrl: 'https://temps.example.test//',
        isLoaded: true,
        origin,
      }).url
    ).toBe('https://temps.example.test/api/_temps')
  })

  it('shows a placeholder while the address is still loading', () => {
    expect(
      analyticsIngestBaseUrl({
        externalUrl: undefined,
        isLoaded: false,
        origin,
      })
    ).toEqual({
      url: 'YOUR_TEMPS_URL/api/_temps',
      source: 'placeholder',
    })
  })

  it('falls back to the console origin only once none is known to be stored', () => {
    expect(
      analyticsIngestBaseUrl({ externalUrl: null, isLoaded: true, origin })
    ).toEqual({
      url: 'http://localhost:3200/api/_temps',
      source: 'console_origin',
    })
    expect(
      analyticsIngestBaseUrl({ externalUrl: '  ', isLoaded: true, origin })
        .source
    ).toBe('console_origin')
  })
})
