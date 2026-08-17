import { describe, expect, test } from 'bun:test'
import { presetIconNeedsDarkInvert, presetIconPath } from './preset-icon-paths'

describe('preset icon appearance', () => {
  test('normalizes preset names when resolving icon paths', () => {
    expect(presetIconPath('docker-compose')).toBe('/presets/docker.svg')
    expect(presetIconPath('Next.js')).toBe('/presets/nextjs.svg')
  })

  test('inverts only monochrome black marks in dark mode', () => {
    expect(presetIconNeedsDarkInvert('Next.js')).toBe(true)
    expect(presetIconNeedsDarkInvert('remix')).toBe(true)
    expect(presetIconNeedsDarkInvert('react')).toBe(false)
    expect(presetIconNeedsDarkInvert('docker')).toBe(false)
  })
})
