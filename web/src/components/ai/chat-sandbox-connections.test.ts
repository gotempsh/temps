// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  connectedSummary,
  connectionLabel,
  connectionState,
  connectionTitle,
} from './chat-sandbox-connections'

describe('connectionState', () => {
  test('is unavailable until a sandbox is linked', () => {
    expect(connectionState(undefined, 3)).toBe('no-sandbox')
    expect(connectionState({ linked: null, connected_projects: [3] }, 3)).toBe(
      'no-sandbox'
    )
  })

  test('tells a connected project from one that can be connected', () => {
    const status = { linked: { name: 's' }, connected_projects: [3] }
    expect(connectionState(status, 3)).toBe('connected')
    expect(connectionState(status, 4)).toBe('available')
    expect(connectionState({ linked: { name: 's' } }, 4)).toBe('available')
  })
})

describe('labels', () => {
  test('the control names the action it will take', () => {
    expect(connectionLabel('available')).toBe('Connect sandbox')
    expect(connectionLabel('no-sandbox')).toBe('Connect sandbox')
    expect(connectionLabel('connected')).toBe('Disconnect sandbox')
  })

  test('every state explains itself, including the one that needs a sandbox', () => {
    expect(connectionTitle('no-sandbox')).toContain('Link a sandbox')
    expect(connectionTitle('available')).toContain('never sees production')
    expect(connectionTitle('connected')).toContain('Disconnect')
  })
})

describe('connectedSummary', () => {
  test('names variables, never values, and counts what was left out', () => {
    const summary = connectedSummary({
      variable_names: ['POSTGRES_URL', 'POSTGRES_PASSWORD', 'POSTGRES_DB'],
      connected_service_ids: [1],
      skipped_service_ids: [2, 5],
    })
    expect(summary).toContain('1 service')
    expect(summary).toContain('POSTGRES_URL, POSTGRES_DB')
    expect(summary).not.toContain('POSTGRES_PASSWORD')
    expect(summary).toContain('2 linked services of another type')
  })

  test('is quiet about services when none were left out', () => {
    const summary = connectedSummary({
      variable_names: [],
      connected_service_ids: [1, 2],
      skipped_service_ids: [],
    })
    expect(summary).toContain('2 services')
    expect(summary).not.toContain('another type')
  })
})
