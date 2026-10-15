// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { ProjectPresetResponse } from '@/api/client'
import {
  importedApp,
  importRowStates,
  defaultSelection,
  fromPublicPreset,
  isNestedApp,
  normalizeDirectory,
  suggestServiceName,
} from './repository-import'

const app = (over: Partial<ProjectPresetResponse>): ProjectPresetResponse => ({
  path: './apps/web',
  preset: 'nextjs',
  presetLabel: 'Next.js',
  projectType: 'frontend',
  ...over,
})

describe('normalizeDirectory', () => {
  test('strips ./ and trailing slashes; the root is "."', () => {
    expect(normalizeDirectory('./apps/web/')).toBe('apps/web')
    expect(normalizeDirectory('apps/web')).toBe('apps/web')
    for (const root of ['./', '.', '', '/', undefined]) {
      expect(normalizeDirectory(root)).toBe('.')
    }
  })
})

describe('suggestServiceName', () => {
  test('uses the folder name, or the repository name at the root', () => {
    expect(suggestServiceName('./apps/web', 'shop', new Set())).toBe('web')
    expect(suggestServiceName('./', 'Shop', new Set())).toBe('shop')
    expect(
      suggestServiceName('services/Billing_Worker', 'shop', new Set())
    ).toBe('billing-worker')
  })

  test('adds a suffix when the name is taken', () => {
    expect(
      suggestServiceName('apps/web', 'shop', new Set(['web', 'web-2']))
    ).toBe('web-3')
  })
})

describe('importedApp', () => {
  test('builds the service from the app directory', () => {
    expect(importedApp(app({ exposedPort: 3000 }), ' web ')).toEqual({
      name: 'web',
      preset: 'nextjs',
      directory: 'apps/web',
      exposed_port: 3000,
      preset_config: undefined,
    })
  })

  test('carries a rolled-up Dockerfile path', () => {
    const body = importedApp(
      app({
        path: './',
        preset: 'dockerfile',
        dockerfilePath: 'docker/api.Dockerfile',
      }),
      'api'
    )
    expect(body.directory).toBe('.')
    expect(body.preset_config).toEqual({
      dockerfilePath: 'docker/api.Dockerfile',
    })
  })
})

describe('importRowStates', () => {
  const step = (
    key: string,
    status: 'pending' | 'done' | 'failed',
    error: string | null = null
  ) => ({
    key,
    label: key,
    status,
    error,
  })
  const services = [
    { template_service: 'web', name: 'web', slug: 'web', project_id: 1 },
    { template_service: 'api', name: 'api', slug: 'api', project_id: null },
  ]

  test('each row follows its own steps', () => {
    const states = importRowStates({
      status: 'failed',
      error: 'slug taken',
      services,
      steps: [
        step('repository', 'done'),
        step('service:web', 'done'),
        step('service:api', 'failed', 'slug taken'),
        step('join:web', 'done'),
        step('join:api', 'pending'),
        step('deploy:web', 'done'),
        step('deploy:api', 'pending'),
      ],
    })
    expect(states).toEqual([
      { state: 'done' },
      { state: 'failed', message: 'slug taken' },
    ])
  })

  test('a row waiting behind a failed step says so', () => {
    const states = importRowStates({
      status: 'failed',
      error: 'repository gone',
      services,
      steps: [
        step('repository', 'failed', 'repository gone'),
        step('service:web', 'pending'),
        step('service:api', 'pending'),
      ],
    })
    expect(states[0]).toEqual({
      state: 'failed',
      message: 'Not added yet: repository gone',
    })
  })

  test('rows of a running import are running', () => {
    const states = importRowStates({
      status: 'running',
      error: null,
      services,
      steps: [
        step('service:web', 'done'),
        step('service:api', 'pending'),
        step('join:web', 'pending'),
        step('join:api', 'pending'),
      ],
    })
    expect(states).toEqual([{ state: 'running' }, { state: 'running' }])
  })
})

describe('fromPublicPreset', () => {
  test('maps the public scan into the connected scan shape', () => {
    expect(
      fromPublicPreset({
        path: 'backend',
        preset: 'dockerfile',
        preset_label: 'Dockerfile',
        project_type: 'server',
        exposed_port: 4000,
        dockerfile_path: null,
        compose_files: null,
        icon_url: null,
      })
    ).toMatchObject({
      path: 'backend',
      preset: 'dockerfile',
      presetLabel: 'Dockerfile',
      projectType: 'server',
      exposedPort: 4000,
    })
  })
})

describe('defaultSelection', () => {
  test('preselects the few top-level apps that have no service yet', () => {
    expect(defaultSelection(['apps/web', 'apps/api'], new Set())).toEqual(
      new Set(['apps/web', 'apps/api'])
    )
    expect(
      defaultSelection(['apps/web', 'apps/api'], new Set(['apps/api']))
    ).toEqual(new Set(['apps/web']))
  })

  test('never preselects an app nested in another one', () => {
    expect(
      defaultSelection(['backend', 'backend/bdd', 'frontend'], new Set())
    ).toEqual(new Set(['backend', 'frontend']))
    expect(isNestedApp('backend/bdd', ['backend', 'backend/bdd'])).toBe(true)
    expect(isNestedApp('backend-go', ['backend', 'backend-go'])).toBe(false)
  })

  test('a large monorepo starts with nothing checked', () => {
    const dirs = [
      'backend',
      'backend-go',
      'docs',
      'frontend',
      'sink',
      'wasm/decoder',
    ]
    expect(defaultSelection(dirs, new Set()).size).toBe(0)
  })
})
