// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { ProjectPresetResponse } from '@/api/client'
import {
  repositoryPresetsForDisplay,
  uniqueRepositoryPresets,
} from './repository-presets'

function preset(
  slug: string,
  path: string,
  label = slug
): ProjectPresetResponse {
  return {
    preset: slug,
    presetLabel: label,
    path,
    projectType: 'server',
  }
}

describe('uniqueRepositoryPresets', () => {
  test('collapses repeated detections from different monorepo paths', () => {
    const result = uniqueRepositoryPresets([
      preset('dockerfile', './'),
      preset('dockerfile', 'apps/api'),
      preset('nextjs', 'apps/web'),
    ])

    expect(result.map((item) => item.preset)).toEqual(['dockerfile', 'nextjs'])
  })

  test('preserves the first detection and handles missing data', () => {
    const root = preset('nextjs', './', 'Next.js')
    expect(uniqueRepositoryPresets([root, preset('nextjs', 'web')])).toEqual([
      root,
    ])
    expect(uniqueRepositoryPresets(null)).toEqual([])
  })
})

describe('repositoryPresetsForDisplay', () => {
  const cached = preset('dockerfile', './')

  test('shows persisted detections before a live scan has run', () => {
    expect(repositoryPresetsForDisplay(undefined, [cached])).toEqual([cached])
  })

  test('replaces stale persisted detections with the live result', () => {
    const next = preset('nextjs', './')
    expect(repositoryPresetsForDisplay([next], [cached])).toEqual([next])
    expect(repositoryPresetsForDisplay([], [cached])).toEqual([])
  })
})
