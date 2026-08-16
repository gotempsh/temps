import { test, expect, describe } from 'bun:test'
import { filterPresetsByType, formatPresetPort } from './index.js'
import type { PresetResponse } from '../../api/types.gen.js'

function preset(overrides: Partial<PresetResponse>): PresetResponse {
  return {
    slug: 'node',
    label: 'Node.js',
    project_type: 'server',
    default_port: 3000,
    description: 'A Node.js server',
    ...overrides,
  } as PresetResponse
}

describe('filterPresetsByType', () => {
  const nodePreset = preset({ slug: 'node', project_type: 'server' })
  const staticPreset = preset({ slug: 'static-site', project_type: 'static' })
  const presets = [nodePreset, staticPreset]

  test('returns every preset when no type filter is given', () => {
    expect(filterPresetsByType(presets, undefined)).toEqual(presets)
  })

  test('matches case-insensitively so --type Server still finds "server" presets', () => {
    expect(filterPresetsByType(presets, 'Server')).toEqual([nodePreset])
    expect(filterPresetsByType(presets, 'STATIC')).toEqual([staticPreset])
  })

  test('returns an empty list rather than throwing for an unknown type', () => {
    expect(filterPresetsByType(presets, 'nonexistent')).toEqual([])
  })
})

describe('formatPresetPort', () => {
  test('renders a real port as a string', () => {
    expect(formatPresetPort(3000)).toBe('3000')
  })

  test('renders a missing port as a dash, not "0" or "null"', () => {
    expect(formatPresetPort(null)).toBe('-')
    expect(formatPresetPort(undefined)).toBe('-')
  })

  test('keeps an actual port 0 distinct from missing', () => {
    expect(formatPresetPort(0)).toBe('0')
  })
})
