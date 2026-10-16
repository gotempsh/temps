// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  AUTO_NODE,
  createSandboxFormDefaults,
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
