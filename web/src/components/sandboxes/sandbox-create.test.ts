// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { NodeBackend, PlacementNode } from '@/api/client'
import {
  AUTO_NODE,
  DEFAULT_BACKEND,
  backendChoices,
  createSandboxFormDefaults,
  nodeRunsBackend,
  targetNode,
  createSandboxFormSchema,
  parsePorts,
  toCreateSandboxBody,
  type CreateSandboxFormValues,
} from './sandbox-create'

const values = (
  overrides: Partial<CreateSandboxFormValues> = {}
): CreateSandboxFormValues => ({ ...createSandboxFormDefaults, ...overrides })

describe('toCreateSandboxBody', () => {
  test('sends only the lifecycle when nothing else is filled in', () => {
    expect(toCreateSandboxBody(values())).toEqual({ lifecycle: 'ephemeral' })
  })

  test('maps every filled field, converting minutes to seconds', () => {
    expect(
      toCreateSandboxBody(
        values({
          name: ' scratch ',
          lifecycle: 'workspace',
          image: 'node:22',
          timeoutMinutes: '120',
          cpuLimit: '1.5',
          memoryLimitMb: '2048',
          pidsLimit: '512',
          ports: '3000, 8080 3000',
          node: 'worker-1',
          previewPassword: 'correct-horse',
        })
      )
    ).toEqual({
      lifecycle: 'workspace',
      name: 'scratch',
      image: 'node:22',
      timeout_secs: 7200,
      cpu_limit: 1.5,
      memory_limit_mb: 2048,
      pids_limit: 512,
      ports: [3000, 8080],
      node: 'worker-1',
      preview_password: 'correct-horse',
    })
  })

  test('automatic placement sends no node', () => {
    expect(
      toCreateSandboxBody(values({ node: AUTO_NODE })).node
    ).toBeUndefined()
  })
})

describe('parsePorts', () => {
  test('accepts commas and spaces and drops duplicates', () => {
    expect(parsePorts('3000,8080 3000')).toEqual([3000, 8080])
    expect(parsePorts('')).toEqual([])
  })

  test('rejects non-numeric and out-of-range ports', () => {
    expect(parsePorts('abc')).toBeNull()
    expect(parsePorts('0')).toBeNull()
    expect(parsePorts('70000')).toBeNull()
  })
})

describe('createSandboxFormSchema', () => {
  test('the defaults are valid', () => {
    expect(
      createSandboxFormSchema.safeParse(createSandboxFormDefaults).success
    ).toBe(true)
  })

  const rejects = (overrides: Partial<CreateSandboxFormValues>) =>
    createSandboxFormSchema.safeParse(values(overrides)).success

  test('rejects out-of-range or malformed limits', () => {
    expect(rejects({ timeoutMinutes: '0' })).toBe(false)
    expect(rejects({ timeoutMinutes: '1441' })).toBe(false)
    expect(rejects({ timeoutMinutes: '30' })).toBe(true)
    expect(rejects({ cpuLimit: 'two' })).toBe(false)
    expect(rejects({ memoryLimitMb: '12' })).toBe(false)
    expect(rejects({ ports: '99999' })).toBe(false)
  })

  test('rejects a short preview password but allows none', () => {
    expect(rejects({ previewPassword: 'short' })).toBe(false)
    expect(rejects({ previewPassword: '' })).toBe(true)
    expect(rejects({ previewPassword: 'long-enough-1' })).toBe(true)
  })

  test('rejects an image reference containing spaces', () => {
    expect(rejects({ image: 'node 22' })).toBe(false)
  })
})

const dockerOnly: NodeBackend[] = [
  { backend: 'docker', available: true, reason: null },
  {
    backend: 'firecracker',
    available: false,
    reason: 'the firecracker backend is not available on worker nodes',
  },
  {
    backend: 'microsandbox',
    available: false,
    reason: 'the microsandbox backend is not available on worker nodes',
  },
]

const node = (overrides: Partial<PlacementNode>): PlacementNode => ({
  id: 3,
  name: 'worker-3',
  is_control_plane: false,
  status: 'active',
  allowed: true,
  eligible: true,
  reason: null,
  live_sandboxes: 0,
  dedicated: false,
  backends: dockerOnly,
  ...overrides,
})

const controlPlane = node({
  id: 0,
  name: 'control-plane',
  is_control_plane: true,
  backends: [
    { backend: 'docker', available: true, reason: null },
    { backend: 'firecracker', available: true, reason: null },
    {
      backend: 'microsandbox',
      available: false,
      reason: 'run `temps microsandbox setup` on the server host',
    },
  ],
})

describe('backend choice', () => {
  test('sends the backend only when one is picked', () => {
    expect(toCreateSandboxBody(values()).backend).toBeUndefined()
    expect(
      toCreateSandboxBody(values({ backend: DEFAULT_BACKEND })).backend
    ).toBeUndefined()
    expect(toCreateSandboxBody(values({ backend: 'firecracker' }))).toEqual({
      lifecycle: 'ephemeral',
      backend: 'firecracker',
    })
  })

  test('automatic placement offers what the control plane runs', () => {
    const choices = backendChoices([controlPlane, node({})], AUTO_NODE)
    expect(choices.map((c) => [c.backend, c.available])).toEqual([
      ['docker', true],
      ['firecracker', true],
      ['microsandbox', false],
    ])
    // The reason is the server's, fix included.
    expect(choices[2].reason).toContain('temps microsandbox setup')
  })

  test('without the control plane, automatic placement offers a worker', () => {
    const excluded = { ...controlPlane, eligible: false }
    const choices = backendChoices([excluded, node({})], AUTO_NODE)
    expect(choices.filter((c) => c.available).map((c) => c.backend)).toEqual([
      'docker',
    ])
    expect(targetNode([excluded, node({})], AUTO_NODE)?.name).toBe('worker-3')
  })

  test('a chosen node offers its own backends', () => {
    const choices = backendChoices([controlPlane, node({})], 'worker-3')
    expect(choices[1]).toEqual(dockerOnly[1])
  })

  test('with no node able to take a sandbox, nothing is offered', () => {
    const choices = backendChoices([{ ...controlPlane, eligible: false }], AUTO_NODE)
    expect(choices.every((c) => !c.available && c.reason)).toBe(true)
  })

  test('a node that reports no backends is left to the server', () => {
    const older = node({ backends: [] })
    expect(backendChoices([older], 'worker-3').every((c) => c.available)).toBe(
      true
    )
    expect(nodeRunsBackend(older, 'firecracker')).toBe(true)
  })

  test('a node runs the default and what it reports as available', () => {
    expect(nodeRunsBackend(node({}), DEFAULT_BACKEND)).toBe(true)
    expect(nodeRunsBackend(node({}), 'docker')).toBe(true)
    expect(nodeRunsBackend(node({}), 'firecracker')).toBe(false)
  })
})
