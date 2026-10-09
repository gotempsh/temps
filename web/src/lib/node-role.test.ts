// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import { nodeSchedulingRole, targetNodesHint } from './node-role'

describe('nodeSchedulingRole', () => {
  test('reads the temps.sh/role label', () => {
    expect(nodeSchedulingRole({ 'temps.sh/role': 'dedicated' })).toBe(
      'dedicated'
    )
    expect(
      nodeSchedulingRole({ 'temps.sh/role': 'builder', gpu: 'true' })
    ).toBe('builder')
  })

  test('is null for an ordinary worker or an unknown role', () => {
    expect(nodeSchedulingRole({})).toBeNull()
    expect(nodeSchedulingRole({ region: 'us' })).toBeNull()
    expect(nodeSchedulingRole({ 'temps.sh/role': 'worker' })).toBeNull()
    expect(nodeSchedulingRole({ 'temps.sh/role': 'Dedicated' })).toBeNull()
  })

  test('ignores look-alike keys and malformed labels', () => {
    expect(nodeSchedulingRole({ 'temps.dedicated': 'true' })).toBeNull()
    expect(nodeSchedulingRole({ role: 'dedicated' })).toBeNull()
    expect(nodeSchedulingRole(null)).toBeNull()
    expect(nodeSchedulingRole(undefined)).toBeNull()
    expect(nodeSchedulingRole('temps.sh/role=dedicated')).toBeNull()
    expect(nodeSchedulingRole(['dedicated'])).toBeNull()
  })
})

describe('targetNodesHint', () => {
  test('keeps the plain hint when no node is dedicated', () => {
    expect(targetNodesHint([{ labels: {} }])).toBe(
      'Restrict deployments to specific nodes. Leave empty to use all active nodes.'
    )
  })

  test('explains that an empty selection skips dedicated nodes', () => {
    const hint = targetNodesHint([
      { labels: {} },
      { labels: { 'temps.sh/role': 'dedicated' } },
    ])
    expect(hint).toContain('except dedicated ones')
    expect(hint).toContain('label selectors never match it')
  })

  test('a builder node alone does not change the hint', () => {
    expect(
      targetNodesHint([{ labels: { 'temps.sh/role': 'builder' } }])
    ).not.toContain('dedicated')
  })
})
