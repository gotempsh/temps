// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  DROP_NO_PROJECT_PROBLEM_TYPE,
  dropDetectionGap,
  prepareAndInspectDrop,
  presetConfigForDropCandidate,
} from './drop-preset-detection'

describe('dropDetectionGap', () => {
  test('returns what Temps looks for when nothing in the archive is deployable', () => {
    expect(
      dropDetectionGap({
        type: DROP_NO_PROJECT_PROBLEM_TYPE,
        title: 'No Deployable Project Found',
        detail: 'No deployable project was found in the archive.',
        supported_files: ['Dockerfile', 'mix.exs', 42],
      })
    ).toEqual(['Dockerfile', 'mix.exs'])
  })

  test('is null for every other failure, which may be worth retrying', () => {
    expect(
      dropDetectionGap({ title: 'Too Many Drop Inspections', status: 429 })
    ).toBeNull()
    expect(dropDetectionGap(new Error('network down'))).toBeNull()
    expect(dropDetectionGap(undefined)).toBeNull()
  })
})

describe('prepareAndInspectDrop', () => {
  test('preserves a detected modern Compose filename for project creation', () => {
    expect(
      presetConfigForDropCandidate({
        composePath: 'compose.yaml',
        confidence: 'high',
        directory: '.',
        isStatic: false,
        label: 'Docker Compose',
        preset: 'docker-compose',
        reason: 'Docker Compose file found',
      })
    ).toEqual({ composePath: 'compose.yaml' })
  })

  test('threads a rolled-up Dockerfile path through to preset_config', () => {
    expect(
      presetConfigForDropCandidate({
        confidence: 'high',
        directory: '.',
        dockerfilePath: 'docker/Dockerfile',
        isStatic: false,
        label: 'Dockerfile',
        preset: 'dockerfile',
        reason: 'Dockerfile found in docker/',
      })
    ).toEqual({ dockerfilePath: 'docker/Dockerfile' })
  })

  test('leaves preset_config unset for a Dockerfile directly at its directory', () => {
    expect(
      presetConfigForDropCandidate({
        confidence: 'high',
        directory: '.',
        dockerfilePath: null,
        isStatic: false,
        label: 'Dockerfile',
        preset: 'dockerfile',
        reason: 'Dockerfile found',
      })
    ).toBeUndefined()
  })

  test('packages selected files and returns the detected preset', async () => {
    let inspectedArchive: File | undefined
    const result = await prepareAndInspectDrop(
      [
        {
          file: new File(['<h1>Hello</h1>'], 'index.html'),
          path: 'index.html',
        },
      ],
      undefined,
      async (archive) => {
        inspectedArchive = archive
        return {
          suggestedName: 'hello',
          candidates: [
            {
              confidence: 'high',
              directory: '.',
              isStatic: true,
              label: 'Static site',
              preset: 'static',
              reason: 'Found index.html',
            },
          ],
        }
      }
    )

    expect(inspectedArchive).toBeInstanceOf(File)
    expect(result.archive).toBe(inspectedArchive!)
    expect(result.inspection.candidates[0]?.preset).toBe('static')
  })

  test('rejects an inspection with no deployable candidates', async () => {
    await expect(
      prepareAndInspectDrop(
        [{ file: new File(['{}'], 'package.json'), path: 'package.json' }],
        undefined,
        async () => ({ suggestedName: 'empty', candidates: [] })
      )
    ).rejects.toThrow('No deployable project preset was detected')
  })

  test('cancels before uploading when the user clears the selection', async () => {
    const controller = new AbortController()
    controller.abort()
    let inspectCalled = false

    await expect(
      prepareAndInspectDrop(
        [{ file: new File(['{}'], 'package.json'), path: 'package.json' }],
        undefined,
        async () => {
          inspectCalled = true
          return { suggestedName: 'cancelled', candidates: [] }
        },
        { signal: controller.signal }
      )
    ).rejects.toMatchObject({ name: 'AbortError' })
    expect(inspectCalled).toBe(false)
  })
})
