// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { ProjectPresetResponse, ProjectResponse } from '@/api/client'
import {
  applyFrameworkSelection,
  dockerfileWithinDirectory,
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

  test('reads a directory the way the server stores it', () => {
    // The server stores `/apps/web` and `apps\\web` as `apps/web`, and the
    // root as `.`; retyping either must not offer a save again.
    const saved = project({ directory: 'apps/web' })
    for (const directory of ['/apps/web', 'apps\\web', ' apps/web ']) {
      expect(
        buildConfigurationPatch(saved, {
          ...buildConfigurationValues(saved),
          directory,
        })
      ).toEqual({})
    }
    const root = project({ directory: '.', pull_only_root_directory: false })
    for (const directory of ['./', '', '/']) {
      expect(
        buildConfigurationPatch(root, {
          ...buildConfigurationValues(root),
          directory,
        })
      ).toEqual({})
    }
    // A leading `./` on a subdirectory is kept, as the server keeps it.
    expect(
      buildConfigurationPatch(saved, {
        ...buildConfigurationValues(saved),
        directory: './apps/web',
      })
    ).toEqual({ directory: './apps/web' })
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

  test('another Dockerfile in the same folder keeps a context above it', () => {
    // The Dockerfile needs files from the repository root, so the context is
    // the root; the new file is rewritten to be found from there.
    const values = {
      ...buildConfigurationValues(project()),
      directory: './apps/web',
      buildContext: '.',
      dockerfilePath: 'apps/web/Dockerfile',
    }
    expect(
      applyFrameworkSelection(
        values,
        'dockerfile::apps/web',
        card({ preset: 'dockerfile', dockerfilePath: 'Dockerfile.worker' })
      )
    ).toEqual({
      ...values,
      dockerfilePath: 'apps/web/Dockerfile.worker',
      buildContext: '.',
    })
  })

  test('a Dockerfile in a folder inside the context keeps it, from any depth', () => {
    const values = buildConfigurationValues(project()) // context ./apps
    expect(
      applyFrameworkSelection(
        values,
        'dockerfile::apps/worker',
        card({ preset: 'dockerfile', dockerfilePath: 'docker/Dockerfile' })
      )
    ).toEqual({
      ...values,
      directory: './apps/worker',
      dockerfilePath: 'worker/docker/Dockerfile',
      buildContext: './apps',
    })
  })

  test('a context equal to the folder is kept, and the file is used as named', () => {
    const values = {
      ...buildConfigurationValues(project()),
      directory: './apps/api',
      buildContext: 'apps/api/',
      dockerfilePath: 'Dockerfile',
    }
    const next = applyFrameworkSelection(
      values,
      'dockerfile::apps/api',
      card({ preset: 'dockerfile', dockerfilePath: 'Dockerfile.worker' })
    )
    expect(next.dockerfilePath).toBe('Dockerfile.worker')
    expect(next.buildContext).toBe('apps/api/')
  })

  test('choosing the current file again under a root context changes nothing', () => {
    const values = {
      ...buildConfigurationValues(project()),
      directory: './apps/web',
      buildContext: '.',
      dockerfilePath: 'apps/web/Dockerfile',
    }
    expect(dockerfileWithinDirectory(values)).toBe('Dockerfile')
    expect(
      applyFrameworkSelection(
        values,
        'dockerfile::apps/web',
        card({ preset: 'dockerfile', dockerfilePath: 'Dockerfile' })
      )
    ).toBe(values)
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
