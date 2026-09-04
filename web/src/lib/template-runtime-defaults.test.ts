// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import {
  historicalImageRuntime,
  serviceTemplateDeployOverrides,
  serviceTemplateRuntimeDefaults,
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
    values.image = `registry.example.test/identity@sha256:${'a'.repeat(64)}`
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

  test('rejects commands that the deployment API cannot execute', () => {
    const values = templateRuntimeDefaults(template)
    values.command = Array.from({ length: 65 }, () => 'arg').join('\n')
    expect(templateRuntimeDefaultsSchema.safeParse(values).success).toBe(false)

    values.command = `start\n${'x'.repeat(1_025)}`
    expect(templateRuntimeDefaultsSchema.safeParse(values).success).toBe(false)
  })

  test('measures image, command, and health limits in UTF-8 bytes', () => {
    const values = templateRuntimeDefaults(template)
    values.image = `${'é'.repeat(225)}@sha256:${'a'.repeat(64)}`
    values.command = 'é'.repeat(513)
    values.healthCheckPath = `/${'é'.repeat(1_024)}`

    const result = templateRuntimeDefaultsSchema.safeParse(values)
    expect(result.success).toBe(false)
    if (!result.success) {
      expect(result.error.issues.map((issue) => issue.path.join('.'))).toEqual(
        expect.arrayContaining(['image', 'command', 'healthCheckPath'])
      )
    }
  })

  test('loads saved image runtime and project resources ahead of catalog defaults', () => {
    expect(
      serviceTemplateRuntimeDefaults(
        {
          preset_config: {
            preset: 'dockerfile',
            imageRuntime: {
              imageRef: 'registry.example.test/identity:27.0.0',
              command: ['start-dev'],
              healthCheckPath: '/health/ready',
            },
          },
          deployment_config: {
            cpuRequest: 750_000,
            cpuLimit: 1_500_000,
            memoryRequest: 768,
            memoryLimit: 2048,
            exposedPort: 9090,
          },
        },
        template
      )
    ).toEqual({
      image: 'registry.example.test/identity:27.0.0',
      command: 'start-dev',
      cpuRequest: '0.75',
      cpuLimit: '1.5',
      memoryRequest: '768',
      memoryLimit: '2048',
      exposedPort: '9090',
      healthCheckPath: '/health/ready',
    })
  })

  test('persists and redeploys the same image runtime without shell parsing', () => {
    const values = templateRuntimeDefaults(template)
    values.image = 'registry.example.test/identity:27.0.0'
    values.command = 'start\n--optimized'

    const presetConfig = {
      preset: 'dockerfile',
      variant: 'custom',
      imageRuntime: {
        imageRef: values.image,
        command: ['start', '--optimized'],
        healthCheckPath: values.healthCheckPath,
      },
    }

    expect(presetConfig).toEqual({
      preset: 'dockerfile',
      variant: 'custom',
      imageRuntime: {
        imageRef: 'registry.example.test/identity:27.0.0',
        command: ['start', '--optimized'],
        healthCheckPath: '/realms/master',
      },
    })
    expect(
      serviceTemplateDeployOverrides({ preset_config: presetConfig })
    ).toEqual({
      image_ref: 'registry.example.test/identity:27.0.0',
      command: ['start', '--optimized'],
      health_check_path: '/realms/master',
    })
  })

  test('keeps an explicit image-default command distinct from legacy fallback', () => {
    const values = templateRuntimeDefaults(template)
    values.command = ''
    const presetConfig = {
      preset: 'dockerfile',
      imageRuntime: {
        imageRef: values.image,
        command: null,
        healthCheckPath: values.healthCheckPath,
      },
    }

    expect(presetConfig).toMatchObject({
      imageRuntime: { command: null },
    })
    expect(
      serviceTemplateRuntimeDefaults({ preset_config: presetConfig }, template)
        .command
    ).toBe('')
    expect(
      serviceTemplateRuntimeDefaults(
        { preset_config: { preset: 'dockerfile' } },
        template
      ).command
    ).toBe('start\n--optimized')
  })

  test('keeps stored runtime editable when the catalog is unavailable', () => {
    expect(
      serviceTemplateRuntimeDefaults(
        {
          preset_config: {
            preset: 'dockerfile',
            imageRuntime: {
              imageRef: 'registry.example.test/identity:27.0.0',
              command: null,
              healthCheckPath: '/ready',
            },
          },
          deployment_config: {
            cpuRequest: 250_000,
            memoryLimit: 1024,
            exposedPort: 8080,
          },
        },
        {}
      )
    ).toEqual({
      image: 'registry.example.test/identity:27.0.0',
      command: '',
      cpuRequest: '0.25',
      cpuLimit: '',
      memoryRequest: '',
      memoryLimit: '1024',
      exposedPort: '8080',
      healthCheckPath: '/ready',
    })
  })

  test('historical redeploy explicitly preserves the image default command', () => {
    expect(
      historicalImageRuntime({
        command: null,
        healthCheckPath: '/ready',
      })
    ).toEqual({ command: [], health_check_path: '/ready' })
    expect(historicalImageRuntime({ command: ['serve'] })).toEqual({
      command: ['serve'],
      health_check_path: '/',
    })
    expect(historicalImageRuntime()).toEqual({
      command: [],
      health_check_path: '/',
    })
  })
})
