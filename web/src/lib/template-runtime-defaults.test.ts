// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import {
  templateRuntimeDefaults,
  templateRuntimeDefaultsSchema,
  templateRuntimeOverrides,
} from './template-runtime-defaults'

const template = {
  image: 'registry.example.test/identity:26.7.2',
  command: ['start', '--optimized'],
  resources: {
    cpu_request: 500_000,
    cpu_limit: 1_000_000,
    memory_request: 512,
    memory_limit: 1536,
  },
  exposed_port: 8080,
  health_check_path: '/realms/master',
}

describe('template runtime defaults', () => {
  test('presents curated runtime values in user-facing units', () => {
    expect(templateRuntimeDefaults(template)).toEqual({
      image: 'registry.example.test/identity:26.7.2',
      command: 'start\n--optimized',
      cpuRequest: '0.5',
      cpuLimit: '1',
      memoryRequest: '512',
      memoryLimit: '1536',
      exposedPort: '8080',
      healthCheckPath: '/realms/master',
    })
  })

  test('serializes edited values for the template creation API', () => {
    const values = templateRuntimeDefaults(template)
    values.image = 'registry.example.test/identity:27.0.0'
    values.cpuRequest = '0.75'
    values.command = ''

    expect(templateRuntimeOverrides(values)).toEqual({
      image: 'registry.example.test/identity:27.0.0',
      command: [],
      cpu_request: 750_000,
      cpu_limit: 1_000_000,
      memory_request: 512,
      memory_limit: 1536,
      exposed_port: 8080,
      health_check_path: '/realms/master',
    })
  })

  test('rejects resource limits below their requests', () => {
    const values = templateRuntimeDefaults(template)
    values.cpuLimit = '0.25'
    values.memoryLimit = '256'

    const result = templateRuntimeDefaultsSchema.safeParse(values)

    expect(result.success).toBe(false)
    if (!result.success) {
      expect(result.error.issues.map((issue) => issue.path.join('.'))).toEqual([
        'cpuLimit',
        'memoryLimit',
      ])
    }
  })
})
