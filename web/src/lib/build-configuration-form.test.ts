// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { ProjectPresetResponse, ProjectResponse } from '@/api/client'
import {
  applyFrameworkSelection,
  buildConfigurationPatch,
  buildConfigurationValues,
  withoutLegacySandboxGrants,
} from './build-configuration-form'

const project = (overrides: Partial<ProjectResponse> = {}) =>
  ({
    id: 7,
    preset: 'dockerfile',
    directory: './apps/api',
    pull_only_root_directory: true,
    preset_config: {
      preset: 'dockerfile',
      dockerfilePath: 'docker/api.Dockerfile',
      buildContext: './apps',
      command: ['worker'],
    },
    ...overrides,
  }) as unknown as ProjectResponse

describe('buildConfigurationPatch', () => {
  test('has nothing to save for the values as loaded', () => {
    const saved = project()
    expect(
      buildConfigurationPatch(saved, buildConfigurationValues(saved))
    ).toEqual({})
  })

  test('ignores whitespace and blanks that resolve to the saved value', () => {
    const saved = project({
      directory: './',
      pull_only_root_directory: false,
      preset_config: {},
    })
    expect(
      buildConfigurationPatch(saved, {
        ...buildConfigurationValues(saved),
        directory: '   ',
        dockerfilePath: ' Dockerfile ',
        buildContext: '  ',
      })
    ).toEqual({})
  })

  test('sends only what changed and keeps the rest of the preset config', () => {
    const saved = project()
    expect(
      buildConfigurationPatch(saved, {
        ...buildConfigurationValues(saved),
        dockerfilePath: 'Dockerfile.prod',
      })
    ).toEqual({
      preset_config: {
        preset: 'dockerfile',
        dockerfilePath: 'Dockerfile.prod',
        buildContext: './apps',
        command: ['worker'],
      },
    })
  })

  test('clearing the build context removes the override', () => {
    const saved = project()
    const patch = buildConfigurationPatch(saved, {
      ...buildConfigurationValues(saved),
      buildContext: '',
    })
    // The server keeps a key the patch omits, so it must be sent as null.
    expect(patch.preset_config).toHaveProperty('buildContext', null)
  })

  test('moving to the repository root turns pull-only off', () => {
    const saved = project()
    expect(
      buildConfigurationPatch(saved, {
        ...buildConfigurationValues(saved),
        directory: './',
      })
    ).toEqual({ directory: './', pull_only_root_directory: false })
  })

  test('saves the Compose file for a Compose project under either spelling', () => {
    for (const preset of ['docker-compose', 'dockercompose']) {
      const saved = project({ preset, preset_config: { publicPorts: [] } })
      expect(
        buildConfigurationPatch(saved, {
          ...buildConfigurationValues(saved),
          composePath: 'compose.yaml',
        })
      ).toEqual({
        preset_config: {
          publicPorts: [],
          preset: 'docker-compose',
          composePath: 'compose.yaml',
        },
      })
    }
  })

  test('sends the preset only when it changed', () => {
    const saved = project({ preset: 'nixpacks', preset_config: {} })
    const values = buildConfigurationValues(saved)
    expect(buildConfigurationPatch(saved, values).preset).toBeUndefined()
    expect(
      buildConfigurationPatch(saved, { ...values, preset: 'nextjs' }).preset
    ).toBe('nextjs')
  })
})

describe('applyFrameworkSelection', () => {
  const card = (fields: Partial<ProjectPresetResponse>) =>
    fields as ProjectPresetResponse
  const current = buildConfigurationValues(
    project({ preset: 'nextjs', preset_config: {} })
  )

  test('a catalog framework keeps the root directory of a monorepo service', () => {
    expect(applyFrameworkSelection(current, 'vite')).toEqual({
      ...current,
      preset: 'vite',
    })
  })

  test('a detected Dockerfile brings its directory and file', () => {
    expect(
      applyFrameworkSelection(
        current,
        'dockerfile::services/web',
        card({ preset: 'dockerfile', dockerfilePath: 'docker/web.Dockerfile' })
      )
    ).toEqual({
      ...current,
      preset: 'dockerfile',
      directory: './services/web',
      dockerfilePath: 'docker/web.Dockerfile',
    })
  })

  test('two Dockerfiles at one path each bring their own file', () => {
    const atRoot = buildConfigurationValues(
      project({
        directory: './',
        preset_config: { dockerfilePath: 'docker/api/Dockerfile' },
      })
    )
    expect(
      applyFrameworkSelection(
        atRoot,
        'dockerfile::root',
        card({ preset: 'dockerfile', dockerfilePath: 'docker/web/Dockerfile' })
      ).dockerfilePath
    ).toBe('docker/web/Dockerfile')
    expect(
      applyFrameworkSelection(
        atRoot,
        'dockerfile::root',
        card({ preset: 'dockerfile', dockerfilePath: null })
      ).dockerfilePath
    ).toBe('Dockerfile')
  })

  test('a detected Compose project at the root brings the file detection found', () => {
    expect(
      applyFrameworkSelection(
        current,
        'docker-compose::root',
        card({ preset: 'docker-compose', composeFiles: ['compose.yaml'] })
      )
    ).toEqual({
      ...current,
      preset: 'docker-compose',
      directory: './',
      composePath: 'compose.yaml',
    })
  })

  test('a Compose file in a subdirectory is stored relative to it', () => {
    const composeFiles = ['docker-compose.yml', 'apps/web/compose.yaml']
    expect(
      applyFrameworkSelection(
        current,
        'docker-compose::apps/web',
        card({ preset: 'docker-compose', composeFiles })
      ).composePath
    ).toBe('compose.yaml')
    expect(
      applyFrameworkSelection(
        current,
        'docker-compose::apps/other',
        card({ preset: 'docker-compose', composeFiles })
      ).composePath
    ).toBe('docker-compose.yml')
  })

  test('choosing what is already selected keeps hand-set paths', () => {
    const saved = project({ directory: './services/web' })
    const values = buildConfigurationValues(saved)
    const next = applyFrameworkSelection(
      values,
      'dockerfile::services/web',
      card({ preset: 'dockerfile', dockerfilePath: 'docker/api.Dockerfile' })
    )
    expect(next).toEqual(values)
    expect(buildConfigurationPatch(saved, next)).toEqual({})

    const compose = project({
      preset: 'docker-compose',
      directory: './',
      preset_config: { composePath: 'deploy/custom.yml' },
    })
    const composeValues = buildConfigurationValues(compose)
    expect(
      applyFrameworkSelection(
        composeValues,
        'docker-compose::root',
        card({ preset: 'docker-compose', composeFiles: ['compose.yaml'] })
      )
    ).toEqual(composeValues)
  })

  test('a detected Dockerfile elsewhere drops the previous build context', () => {
    const values = buildConfigurationValues(project())
    expect(
      applyFrameworkSelection(
        values,
        'dockerfile::services/web',
        card({ preset: 'dockerfile', dockerfilePath: 'docker/web.Dockerfile' })
      )
    ).toEqual({
      ...values,
      directory: './services/web',
      dockerfilePath: 'docker/web.Dockerfile',
      buildContext: '',
    })
  })

  test('another detected Dockerfile beside the current one drops the build context', () => {
    // Detection names the file from the repository root, so keeping a
    // subdirectory context would make the build look for apps/docker/Dockerfile.
    const values = {
      ...buildConfigurationValues(project()),
      directory: './',
      buildContext: './apps',
    }
    expect(
      applyFrameworkSelection(
        values,
        'dockerfile::root',
        card({ preset: 'dockerfile', dockerfilePath: 'docker/Dockerfile' })
      )
    ).toEqual({
      ...values,
      preset: 'dockerfile',
      directory: './',
      dockerfilePath: 'docker/Dockerfile',
      buildContext: '',
    })
  })

  test('a detected Dockerfile with no named file falls back to the default', () => {
    const values = { ...current, dockerfilePath: 'stale.Dockerfile' }
    expect(
      applyFrameworkSelection(
        values,
        'dockerfile::services/other',
        card({ preset: 'dockerfile' })
      ).dockerfilePath
    ).toBe('Dockerfile')
  })
})

describe('withoutLegacySandboxGrants', () => {
  test('leaving Compose clears grants the server would refuse', () => {
    const stored = { composePath: 'compose.yml', unsandboxedServices: ['db'] }
    expect(withoutLegacySandboxGrants('dockerfile', stored)).toEqual({
      composePath: 'compose.yml',
      unsandboxedServices: [],
    })
    expect(
      withoutLegacySandboxGrants('nextjs', stored).unsandboxedServices
    ).toEqual([])
  })

  test('Compose always sends the empty list, since an omitted key is kept', () => {
    expect(withoutLegacySandboxGrants('docker-compose', {})).toEqual({
      unsandboxedServices: [],
    })
  })

  test('another framework with no grants is sent as it is', () => {
    const stored = { dockerfilePath: 'Dockerfile' }
    expect(withoutLegacySandboxGrants('dockerfile', stored)).toBe(stored)
    const empty = { unsandboxedServices: [] }
    expect(withoutLegacySandboxGrants('dockerfile', empty)).toBe(empty)
  })
})
