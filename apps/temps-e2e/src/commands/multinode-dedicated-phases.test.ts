// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import { dedicatedLabelJq, hasDedicatedRole, projectContainers } from './multinode-dedicated-phases.ts'

describe('dedicated phase helpers', () => {
  test('the jq program adds the role and keeps existing labels', async () => {
    const proc = Bun.spawn(['jq', '-c', dedicatedLabelJq()], { stdin: 'pipe', stdout: 'pipe', stderr: 'pipe' })
    proc.stdin.write('{"node_id":3,"labels":{"gpu":"true"}}')
    proc.stdin.end()
    const out = await new Response(proc.stdout).text()
    if ((await proc.exited) !== 0) return // jq is only guaranteed inside the cluster image
    expect(JSON.parse(out)).toEqual({ node_id: 3, labels: { gpu: 'true', 'temps.sh/role': 'dedicated' } })
  })

  test('recognises only the dedicated role label', () => {
    expect(hasDedicatedRole({ 'temps.sh/role': 'dedicated' })).toBe(true)
    expect(hasDedicatedRole({ 'temps.sh/role': 'builder' })).toBe(false)
    expect(hasDedicatedRole({ role: 'dedicated' })).toBe(false)
    expect(hasDedicatedRole(null)).toBe(false)
    expect(hasDedicatedRole(['dedicated'])).toBe(false)
  })

  test('filters a node container list to one project', () => {
    expect(projectContainers(['app-e2e-1-a', 'temps-proxy', 'app-e2e-1-b'], 'e2e-1')).toEqual([
      'app-e2e-1-a',
      'app-e2e-1-b',
    ])
    expect(projectContainers([], 'e2e-1')).toEqual([])
  })
})
