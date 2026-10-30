// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { containerRuntime, containerRuntimeLabel } from './container-runtime'

describe('containerRuntime', () => {
  test('reports the runtime the API names', () => {
    expect(containerRuntime({ runtime: 'firecracker' })).toBe('firecracker')
    expect(containerRuntime({ runtime: 'Docker' })).toBe('docker')
  })

  test('a container from a server without the field is a Docker container', () => {
    expect(containerRuntime({})).toBe('docker')
    expect(containerRuntime({ runtime: null })).toBe('docker')
    expect(containerRuntime({ runtime: '  ' })).toBe('docker')
    expect(containerRuntime(null)).toBe('docker')
  })
})

describe('containerRuntimeLabel', () => {
  test('names a microVM as one', () => {
    expect(containerRuntimeLabel('docker')).toBe('Docker')
    expect(containerRuntimeLabel('firecracker')).toBe('Firecracker microVM')
  })

  test('an unknown runtime is shown as reported', () => {
    expect(containerRuntimeLabel('qemu')).toBe('qemu')
  })
})
