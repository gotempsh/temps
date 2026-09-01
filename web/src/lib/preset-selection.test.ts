// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  normalizePresetPath,
  presetConfigForSelection,
  presetSelectionKey,
  splitPresetSelection,
} from './preset-selection'

describe('presetConfigForSelection', () => {
  test('selecting base Nixpacks resets providers but preserves inline TOML', () => {
    expect(
      presetConfigForSelection('nixpacks', {
        preset: 'nixpacks',
        providers: ['node'],
        nixpacksConfig: '[start]\ncmd = "npm start"',
      })
    ).toEqual({
      preset: 'nixpacks',
      providers: [],
      nixpacksConfig: '[start]\ncmd = "npm start"',
    })
  })

  test('does not carry another preset config into Nixpacks', () => {
    expect(
      presetConfigForSelection('nixpacks', {
        preset: 'dockerfile',
        dockerfilePath: 'Dockerfile.api',
      })
    ).toEqual({ providers: [] })
  })

  test('leaves provider-specific selections unchanged', () => {
    const config = { preset: 'nixpacks', providers: ['python'] }
    expect(presetConfigForSelection('nixpacks-python', config)).toBe(config)
  })
})

/**
 * These helpers exist because the composite `slug::path` key was being handled
 * by hand in two places and drifted: the selector's value was submitted to the
 * API verbatim ("Unknown preset: dockerfile::examples/go-error-tracking") and a
 * bare slug made every card sharing that slug render as selected. The cases
 * below are the shapes that actually broke.
 */
describe('normalizePresetPath', () => {
  test.each([
    ['./', 'root'],
    ['.', 'root'],
    ['', 'root'],
    ['   ', 'root'],
    [undefined, 'root'],
    [null, 'root'],
  ] as Array<[string | null | undefined, string]>)(
    'treats %p as the repository root',
    (input, expected) => {
      expect(normalizePresetPath(input)).toBe(expected)
    }
  )

  test('strips a leading ./ so configured and detected paths compare equal', () => {
    expect(normalizePresetPath('./examples/go-error-tracking')).toBe(
      'examples/go-error-tracking'
    )
    expect(normalizePresetPath('examples/go-error-tracking')).toBe(
      'examples/go-error-tracking'
    )
  })
})

describe('presetSelectionKey', () => {
  test('pairs the slug with the root marker when there is no subdirectory', () => {
    expect(presetSelectionKey('dockerfile', './')).toBe('dockerfile::root')
    expect(presetSelectionKey('dockerfile', undefined)).toBe('dockerfile::root')
  })

  test('carries the subdirectory so two presets at different paths differ', () => {
    const a = presetSelectionKey('dockerfile', 'examples/go-error-tracking')
    const b = presetSelectionKey('dockerfile', 'examples/docker/go-services')
    expect(a).toBe('dockerfile::examples/go-error-tracking')
    expect(a).not.toBe(b)
  })

  test('returns empty for a missing preset rather than a dangling separator', () => {
    expect(presetSelectionKey(undefined, './')).toBe('')
    expect(presetSelectionKey('', './')).toBe('')
  })
})

describe('splitPresetSelection', () => {
  test('splits the key the API rejected back into preset + directory', () => {
    expect(
      splitPresetSelection('dockerfile::examples/go-error-tracking')
    ).toEqual({
      preset: 'dockerfile',
      directory: './examples/go-error-tracking',
    })
  })

  test('maps the root marker to the API’s "./"', () => {
    expect(splitPresetSelection('dockerfile::root')).toEqual({
      preset: 'dockerfile',
      directory: './',
    })
  })

  test('tolerates a bare slug, treating it as the repository root', () => {
    expect(splitPresetSelection('dockerfile')).toEqual({
      preset: 'dockerfile',
      directory: './',
    })
  })

  test('passes "custom" through with a root directory', () => {
    expect(splitPresetSelection('custom')).toEqual({
      preset: 'custom',
      directory: './',
    })
  })
})

describe('round trip', () => {
  test.each([
    ['dockerfile', './'],
    ['dockerfile', './examples/go-error-tracking'],
    ['go', './examples/docker/go-services'],
    ['nextjs', './'],
  ] as Array<[string, string]>)(
    'key(%s, %s) survives a split back to the same pair',
    (preset, directory) => {
      const key = presetSelectionKey(preset, directory)
      expect(splitPresetSelection(key)).toEqual({ preset, directory })
    }
  )

  test('normalises an unprefixed directory on the way through', () => {
    // The API sometimes stores `examples/foo` and sometimes `./examples/foo`;
    // both must land on the same key and come back in the API's shape.
    const key = presetSelectionKey('dockerfile', 'examples/foo')
    expect(key).toBe(presetSelectionKey('dockerfile', './examples/foo'))
    expect(splitPresetSelection(key).directory).toBe('./examples/foo')
  })
})
