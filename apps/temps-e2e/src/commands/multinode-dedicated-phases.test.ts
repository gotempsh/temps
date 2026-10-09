// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import {
  hasDedicatedRole,
  projectContainers,
  teardownTracked,
  withDedicatedRole,
  type TrackedResources,
} from './multinode-dedicated-phases.ts'

describe('withDedicatedRole', () => {
  test('adds the role and keeps every other key and label', () => {
    const agentJson = JSON.stringify({
      node_id: 3,
      control_plane_url: 'https://control-plane.test',
      labels: { gpu: 'true', zone: 'a' },
    })
    expect(JSON.parse(withDedicatedRole(agentJson))).toEqual({
      node_id: 3,
      control_plane_url: 'https://control-plane.test',
      labels: { gpu: 'true', zone: 'a', 'temps.sh/role': 'dedicated' },
    })
  })

  test('creates the labels object when the config has none', () => {
    expect(JSON.parse(withDedicatedRole('{"node_id":3}')).labels).toEqual({ 'temps.sh/role': 'dedicated' })
    expect(JSON.parse(withDedicatedRole('{"node_id":3,"labels":null}')).labels).toEqual({
      'temps.sh/role': 'dedicated',
    })
  })

  test('replaces another role rather than adding a second one', () => {
    const labels = JSON.parse(withDedicatedRole('{"labels":{"temps.sh/role":"builder"}}')).labels
    expect(labels).toEqual({ 'temps.sh/role': 'dedicated' })
  })

  test('is idempotent and ends with a newline', () => {
    const once = withDedicatedRole('{"labels":{"gpu":"true"}}')
    expect(withDedicatedRole(once)).toBe(once)
    expect(once.endsWith('}\n')).toBe(true)
  })

  test('refuses anything that is not an agent config', () => {
    expect(() => withDedicatedRole('not json')).toThrow()
    expect(() => withDedicatedRole('[]')).toThrow('not a JSON object')
    expect(() => withDedicatedRole('null')).toThrow('not a JSON object')
    expect(() => withDedicatedRole('{"labels":["gpu"]}')).toThrow('"labels" is not an object')
    expect(() => withDedicatedRole('{"labels":"gpu=true"}')).toThrow('"labels" is not an object')
  })
})

describe('teardownTracked', () => {
  const tracked = (): TrackedResources => ({
    deployments: [{ projectId: 1, deploymentId: 10 }],
    projectIds: [1, 2],
  })

  test('forgets the resources once teardown succeeds', async () => {
    const resources = tracked()
    const seen: TrackedResources[] = []
    const errors = await teardownTracked(resources, async (r) => {
      seen.push(r)
      return { errors: [] }
    })
    expect(errors).toEqual([])
    expect(seen).toEqual([tracked()])
    expect(resources).toEqual({ deployments: [], projectIds: [] })
  })

  test('keeps every record after a failed teardown so a retry covers them', async () => {
    const resources = tracked()
    const errors = await teardownTracked(resources, async () => ({ errors: ['deleteProject(2): HTTP 500'] }))
    expect(errors).toEqual(['deleteProject(2): HTTP 500'])
    expect(resources).toEqual(tracked())

    const retried: TrackedResources[] = []
    await teardownTracked(resources, async (r) => {
      retried.push(r)
      return { errors: [] }
    })
    expect(retried).toEqual([tracked()])
    expect(resources).toEqual({ deployments: [], projectIds: [] })
  })

  test('hands teardown a copy, not the live tracking arrays', async () => {
    const resources = tracked()
    await teardownTracked(resources, async (r) => {
      expect(r.deployments).not.toBe(resources.deployments)
      expect(r.projectIds).not.toBe(resources.projectIds)
      return { errors: ['still failing'] }
    })
    expect(resources).toEqual(tracked())
  })

  test('does nothing when nothing is tracked', async () => {
    let called = false
    const errors = await teardownTracked({ deployments: [], projectIds: [] }, async () => {
      called = true
      return { errors: [] }
    })
    expect(errors).toEqual([])
    expect(called).toBe(false)
  })
})

describe('dedicated phase helpers', () => {
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
