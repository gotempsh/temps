// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import { filterPresetsByType, unavailablePresets } from './index.js'
import type { PresetResponse } from '../../api/types.gen.js'

function makePreset(overrides: Partial<PresetResponse> = {}): PresetResponse {
  return {
    slug: 'node',
    label: 'Node.js',
    description: 'A Node.js server',
    icon_url: '',
    project_type: 'server',
    default_port: 3000,
    available: true,
    unavailable_reason: null,
    ...overrides,
  }
}

describe('filterPresetsByType', () => {
  test('returns everything untouched when no type filter is given', () => {
    const presets = [makePreset({ project_type: 'server' }), makePreset({ project_type: 'static' })]
    expect(filterPresetsByType(presets)).toBe(presets)
  })

  test('matches project_type case-insensitively', () => {
    // --type Server and --type server must behave the same, or a script
    // that title-cases its input gets silently zero results.
    const presets = [makePreset({ project_type: 'server' })]
    expect(filterPresetsByType(presets, 'Server')).toHaveLength(1)
    expect(filterPresetsByType(presets, 'SERVER')).toHaveLength(1)
  })

  test('excludes presets of a different type', () => {
    const presets = [makePreset({ project_type: 'server' }), makePreset({ project_type: 'static' })]
    expect(filterPresetsByType(presets, 'static')).toHaveLength(1)
  })

  test('returns an empty array for a type that matches nothing', () => {
    const presets = [makePreset({ project_type: 'server' })]
    expect(filterPresetsByType(presets, 'worker')).toEqual([])
  })
})

describe('unavailablePresets', () => {
  test('lists only presets the server marks unavailable, with its reason', () => {
    const presets = [
      makePreset({ slug: 'node' }),
      makePreset({
        slug: 'docker-compose',
        available: false,
        unavailable_reason: 'Docker Compose projects are not supported while Temps runs in a container.',
      }),
    ]
    expect(unavailablePresets(presets)).toEqual([
      {
        slug: 'docker-compose',
        reason: 'Docker Compose projects are not supported while Temps runs in a container.',
      },
    ])
  })

  test('says something even when the server gives no reason', () => {
    const presets = [makePreset({ slug: 'x', available: false, unavailable_reason: null })]
    expect(unavailablePresets(presets)[0]?.reason).toBe('Not available on this instance.')
  })

  test('is empty when every preset is available', () => {
    expect(unavailablePresets([makePreset()])).toEqual([])
  })
})
