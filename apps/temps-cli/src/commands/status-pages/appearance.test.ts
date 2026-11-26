// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { registerStatusPagesCommands } from './index.js'
import { runWithApi } from '../../lib/mock-api.js'

const page = { id: 3, title: 'Status', description: null, is_published: false, projects: [], monitors: [], appearance: {} }

describe('status page appearance', () => {
  test('create sends the appearance given', async () => {
    const run = await runWithApi(
      registerStatusPagesCommands,
      ['status-pages', 'create', '-t', 'Status', '-m', '5=API', '--theme', 'dark', '--accent-color', '#2563eb', '--no-show-response-times', '--json'],
      { 'POST /status-pages': page },
    )
    expect(run.error).toBeUndefined()
    expect(run.bodies[0]?.body).toMatchObject({
      title: 'Status',
      appearance: { theme: 'dark', accent_color: '#2563eb', show_response_times: false },
    })
  })

  test('update changes only the appearance; no appearance flag sends none', async () => {
    const run = await runWithApi(registerStatusPagesCommands, ['status-pages', 'update', '--id', '3', '--logo-url', 'https://example.com/logo.svg', '--json'], {
      'PATCH /status-pages/3': page,
    })
    expect(run.error).toBeUndefined()
    expect(run.bodies[0]?.body).toEqual({ appearance: { logo_url: 'https://example.com/logo.svg' } })
    const plain = await runWithApi(registerStatusPagesCommands, ['status-pages', 'update', '--id', '3', '-t', 'New', '--json'], {
      'PATCH /status-pages/3': page,
    })
    expect(plain.bodies[0]?.body).toEqual({ title: 'New' })
    const bad = await runWithApi(registerStatusPagesCommands, ['status-pages', 'update', '--id', '3', '--theme', 'neon'], {})
    expect(bad.error?.message).toMatch(/--theme must be one of auto, light, dark/)
    expect(bad.requests).toEqual([])
  })
})
