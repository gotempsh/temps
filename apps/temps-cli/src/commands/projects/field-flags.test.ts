// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { registerProjectsCommands } from './index.js'
import { runWithApi } from '../../lib/mock-api.js'

const project = {
  id: 7,
  slug: 'web',
  name: 'web',
  main_branch: 'main',
  directory: '.',
  preset: 'nextjs',
  repo_owner: 'acme',
  repo_name: 'web',
  git_provider_connection_id: null,
}
const lookup = { 'GET /projects/by-slug/web': project }

describe('projects request fields', () => {
  test('settings sends the extra settings without asking anything', async () => {
    const run = await runWithApi(
      registerProjectsCommands,
      ['projects', 'settings', '-p', 'web', '--shield-mode', 'log', '--preview-envs-on-demand', '--log-ingest-lines-per-sec', '500', '--json'],
      { ...lookup, 'POST /projects/7/settings': project },
      { stdin: '' },
    )
    expect(run.error).toBeUndefined()
    expect(run.bodies[0]?.body).toEqual({ shield_mode: 'log', preview_envs_on_demand: true, log_ingest_lines_per_sec: 500 })
  })

  test('update sends build settings and switches', async () => {
    const run = await runWithApi(
      registerProjectsCommands,
      ['projects', 'update', '-p', 'web', '--yes', '--build-command', 'bun run build', '--output-dir', 'dist', '--no-is-web-app', '--preset-config', '{"buildCommand":"bun run build"}', '--json'],
      { ...lookup, 'PUT /projects/7': project },
    )
    expect(run.error).toBeUndefined()
    expect(run.bodies[0]?.body).toMatchObject({
      name: 'web',
      build_command: 'bun run build',
      output_dir: 'dist',
      is_web_app: false,
      preset_config: { buildCommand: 'bun run build' },
    })
  })

  test('config sends deployment settings without asking anything', async () => {
    const run = await runWithApi(
      registerProjectsCommands,
      ['projects', 'config', '-p', 'web', '--exposed-port', '8080', '--build-location', 'node', '--health', '{"path":"/healthz"}', '--json'],
      { ...lookup, 'PATCH /projects/7/deployment-config': {} },
      { stdin: '' },
    )
    expect(run.error).toBeUndefined()
    expect(run.bodies[0]?.body).toEqual({ exposedPort: 8080, buildLocation: 'node', health: { path: '/healthz' } })
  })

  test('an invalid value fails before any request', async () => {
    const run = await runWithApi(registerProjectsCommands, ['projects', 'config', '-p', 'web', '--build-location', 'moon'], {})
    expect(run.error?.message).toMatch(/--build-location must be one of control_plane, node, ci/)
    expect(run.requests).toEqual([])
  })

  test('create sends the switches for the new service', async () => {
    const run = await runWithApi(
      registerProjectsCommands,
      ['projects', 'create', '--external', '--name', 'site', '--performance-metrics-enabled', '--expected-slug', 'site', '--yes'],
      { 'POST /projects': { id: 8, slug: 'site', name: 'site' } },
    )
    expect(run.error).toBeUndefined()
    expect(run.bodies[0]?.body).toMatchObject({ name: 'site', hosting: 'external', performance_metrics_enabled: true, expected_slug: 'site' })
  })
})

describe('projects git request fields', () => {
  test('sends sparse checkout and preset settings with the repository', async () => {
    const run = await runWithApi(
      registerProjectsCommands,
      ['projects', 'git', '-p', 'web', '--yes', '--pull-only-root-directory', '--preset-config', '{"outputDir":"out"}', '--json'],
      { ...lookup, 'POST /projects/7/git': project },
    )
    expect(run.error).toBeUndefined()
    expect(run.bodies.at(-1)?.body).toMatchObject({
      repo_owner: 'acme',
      repo_name: 'web',
      pull_only_root_directory: true,
      preset_config: { outputDir: 'out' },
    })
  })
})
