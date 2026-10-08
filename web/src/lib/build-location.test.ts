// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import {
  buildLocationToPayload,
  buildLocationToSelect,
} from './build-location'

describe('buildLocationToSelect', () => {
  test('an unset environment follows the project', () => {
    expect(buildLocationToSelect(undefined)).toBe('inherit')
    expect(buildLocationToSelect(null)).toBe('inherit')
  })

  test('an explicit override is shown as-is', () => {
    expect(buildLocationToSelect('node')).toBe('node')
    expect(buildLocationToSelect('control_plane')).toBe('control_plane')
  })
})

describe('buildLocationToPayload', () => {
  test('inherit sends null so saving clears the override', () => {
    const body = { build_location: buildLocationToPayload('inherit') }

    expect(JSON.stringify(body)).toBe('{"build_location":null}')
  })

  test('an explicit location is sent unchanged', () => {
    expect(buildLocationToPayload('node')).toBe('node')
    expect(buildLocationToPayload('control_plane')).toBe('control_plane')
  })
})
