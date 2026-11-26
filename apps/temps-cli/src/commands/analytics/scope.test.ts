// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { registerAnalyticsCommands } from './index.js'
import { registerFunnelsCommands } from '../funnels/index.js'
import { runWithApi } from '../../lib/mock-api.js'

const project = { 'GET /projects/by-slug/web': { id: 7, slug: 'web', name: 'web' } }

describe('analytics scope and breakdown filters', () => {
  test('top passes the environment, deployment and dimension filters', async () => {
    const run = await runWithApi(
      registerAnalyticsCommands,
      ['analytics', 'top', 'pages', '-p', 'web', '--environment-id', '2', '--deployment-id', '9', '--filter-os', 'Linux', '--include-crawlers', '--json'],
      { ...project, 'GET /projects/7/events/properties/breakdown': { items: [] } },
    )
    expect(run.error).toBeUndefined()
    const query = new URLSearchParams(run.requests.at(-1)?.split('?')[1])
    expect([query.get('environment_id'), query.get('deployment_id'), query.get('filter_os'), query.get('include_crawlers')]).toEqual(['2', '9', 'Linux', 'true'])
  })

  test('an invalid environment ID fails before any request', async () => {
    const run = await runWithApi(registerAnalyticsCommands, ['analytics', 'top', 'pages', '-p', 'web', '--environment-id', 'prod'], project)
    expect(run.error?.message).toMatch(/--environment-id must be a whole number/)
    expect(run.requests).toEqual([])
  })

  test('funnels metrics passes the window, environment and country', async () => {
    const run = await runWithApi(
      registerFunnelsCommands,
      ['funnels', 'metrics', '--project-id', '7', '--funnel-id', '3', '--environment-id', '2', '--country-code', 'DE', '--json'],
      { 'GET /projects/7/funnels/3/metrics': { steps: [] } },
    )
    expect(run.error).toBeUndefined()
    expect(run.requests).toEqual(['GET /projects/7/funnels/3/metrics?environment_id=2&country_code=DE'])
  })
})
