// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { detectedPortForSelection } from './dockerfile-port'

describe('detectedPortForSelection', () => {
  const presets = [
    {
      preset: 'dockerfile',
      path: './',
      exposedPort: 3000,
    },
    {
      preset: 'dockerfile',
      path: 'apps/api',
      exposed_port: 8080,
    },
  ]

  test('uses the port from the selected Dockerfile directory', () => {
    expect(detectedPortForSelection(presets, 'dockerfile::apps/api')).toBe(8080)
  })

  test('normalizes the root directory selector', () => {
    expect(detectedPortForSelection(presets, 'dockerfile::root')).toBe(3000)
  })

  test('supports a bare preset selection when only the slug is available', () => {
    expect(detectedPortForSelection(presets, 'dockerfile')).toBe(3000)
  })

  test('rejects missing and invalid detected ports', () => {
    expect(
      detectedPortForSelection(
        [{ preset: 'dockerfile', path: './', exposedPort: 0 }],
        'dockerfile::root'
      )
    ).toBeUndefined()
    expect(
      detectedPortForSelection(presets, 'dockerfile::apps/worker')
    ).toBeUndefined()
  })

  test('accepts only finite TCP port boundaries', () => {
    expect(
      detectedPortForSelection(
        [{ preset: 'dockerfile', path: './', exposedPort: 65535 }],
        'dockerfile::root'
      )
    ).toBe(65535)
    for (const exposedPort of [-1, 65536, Number.NaN]) {
      expect(
        detectedPortForSelection(
          [{ preset: 'dockerfile', path: './', exposedPort }],
          'dockerfile::root'
        )
      ).toBeUndefined()
    }
  })

  test('normalizes a trailing slash in nested preset paths', () => {
    expect(
      detectedPortForSelection(presets, 'dockerfile::apps/api/')
    ).toBe(8080)
  })
})
