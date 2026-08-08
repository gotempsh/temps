import { test, expect, describe } from 'bun:test'
import { filterPresetsByType } from './index.js'
import type { PresetResponse } from '../../api/types.gen.js'

function makePreset(overrides: Partial<PresetResponse> = {}): PresetResponse {
  return {
    slug: 'node',
    label: 'Node.js',
    description: 'A Node.js server',
    icon_url: '',
    project_type: 'server',
    default_port: 3000,
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
