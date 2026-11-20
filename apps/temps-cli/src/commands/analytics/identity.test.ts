// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { describeIdentity, parseBrowserIdentify, parseIdentityMode } from './identity'

describe('parseIdentityMode', () => {
  test('accepts both modes, case-insensitively', () => {
    expect(parseIdentityMode('persistent')).toBe('persistent')
    expect(parseIdentityMode(' Cookieless ')).toBe('cookieless')
  })

  test('names the accepted values when the mode is unknown', () => {
    expect(() => parseIdentityMode('anonymous')).toThrow(
      '--mode must be one of persistent, cookieless; got "anonymous"',
    )
  })
})

describe('describeIdentity', () => {
  const updated_at = '2026-11-20T10:00:00Z'

  test('persistent mode has no tracker option and no limitations', () => {
    expect(
      describeIdentity({
        project_id: 3,
        mode: 'persistent',
        accept_browser_identify: true,
        sdk_option: null,
        limitations: [],
        updated_at,
      }),
    ).toEqual([
      'Mode: persistent (visitor id kept in a first-party cookie or localStorage)',
      'Browser identify: accepted (identify() from the page labels its own visitor)',
    ])
  })

  test('cookieless mode names the tracker option and every limitation', () => {
    const lines = describeIdentity({
      project_id: 3,
      mode: 'cookieless',
      accept_browser_identify: true,
      sdk_option: 'cookieless: true',
      limitations: ['A returning visitor counts as a new visitor the next day.'],
      updated_at,
    })
    expect(lines[0]).toContain('cookieless')
    expect(lines[1]).toBe('Tracker option: cookieless: true (script tag: data-cookieless="true")')
    expect(lines[2]).toBe('Identify: stores nothing in cookieless mode, from the browser or the backend')
    expect(lines[3]).toBe('- A returning visitor counts as a new visitor the next day.')
  })

  test('says when browser identify is refused', () => {
    const lines = describeIdentity({
      project_id: 3,
      mode: 'persistent',
      accept_browser_identify: false,
      sdk_option: null,
      limitations: [],
      updated_at,
    })
    expect(lines[1]).toBe("Browser identify: refused (only the app's backend can identify a visitor)")
  })
})

describe('parseBrowserIdentify', () => {
  test('maps accept and refuse', () => {
    expect(parseBrowserIdentify('accept')).toBe(true)
    expect(parseBrowserIdentify(' Refuse ')).toBe(false)
  })

  test('names the accepted values', () => {
    expect(() => parseBrowserIdentify('yes')).toThrow(
      '--browser-identify must be accept or refuse; got "yes"',
    )
  })
})
