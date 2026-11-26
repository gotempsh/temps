// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { resolveHandoffOrigin } from './handoff-origin'

const BROWSER = 'http://localhost:8080'

describe('resolveHandoffOrigin', () => {
  test('decides nothing while the settings read is in flight', () => {
    expect(
      resolveHandoffOrigin({
        externalUrl: undefined,
        settled: false,
        browserOrigin: BROWSER,
      })
    ).toBeNull()
  })

  test("prefers the operator's external URL over the address bar", () => {
    expect(
      resolveHandoffOrigin({
        externalUrl: 'https://temps.example.com/',
        settled: true,
        browserOrigin: BROWSER,
      })
    ).toEqual({ origin: 'https://temps.example.com', source: 'external_url' })
  })

  test('uses the address bar when no external URL is set', () => {
    for (const externalUrl of [null, undefined, '', '   ']) {
      expect(
        resolveHandoffOrigin({
          externalUrl,
          settled: true,
          browserOrigin: BROWSER,
        })
      ).toEqual({ origin: BROWSER, source: 'browser' })
    }
  })

  test('uses the address bar when the person may not read settings', () => {
    // A refused read settles with no data: an answer, not a reason to wait.
    expect(
      resolveHandoffOrigin({
        externalUrl: undefined,
        settled: true,
        browserOrigin: BROWSER,
      })
    ).toEqual({ origin: BROWSER, source: 'browser' })
  })

  test('ignores an external URL that is not an http(s) address', () => {
    for (const externalUrl of [
      'temps.example.com',
      'ftp://temps.example.com',
      'not a url',
    ]) {
      expect(
        resolveHandoffOrigin({
          externalUrl,
          settled: true,
          browserOrigin: BROWSER,
        })
      ).toEqual({ origin: BROWSER, source: 'browser' })
    }
  })
})
