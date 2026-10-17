// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { containerWorkload, portlessWorkerHealth } from './container-workload'

test('retained history without port metadata is unknown, including Compose HTTP containers', () => {
  expect(containerWorkload({})).toBe('unknown')
  expect(containerWorkload(undefined)).toBe('unknown')
  expect(containerWorkload({ container_port: 8080 })).toBe('service')
  expect(containerWorkload({ container_port: null })).toBe('worker')
})

test('project worker health requires explicit portless metadata for every container', () => {
  expect(portlessWorkerHealth(undefined)).toBeNull()
  expect(portlessWorkerHealth([])).toBeNull()
  expect(portlessWorkerHealth([{ health_state: 'healthy' }])).toBeNull()
  expect(
    portlessWorkerHealth([
      { container_port: null, health_state: 'healthy' },
      { container_port: 8080, health_state: 'healthy' },
    ])
  ).toBeNull()
})

test('project worker health combines recorded health without turning unknown into healthy', () => {
  expect(
    portlessWorkerHealth([{ container_port: null, health_state: 'healthy' }])
  ).toBe('healthy')
  expect(
    portlessWorkerHealth([
      { container_port: null, health_state: 'healthy' },
      { container_port: null, health_state: 'unknown' },
    ])
  ).toBe('unknown')
  expect(
    portlessWorkerHealth([
      { container_port: null, health_state: 'healthy' },
      { container_port: null, health_state: 'unhealthy' },
    ])
  ).toBe('unhealthy')
  expect(portlessWorkerHealth([{ container_port: null }])).toBe('unknown')
})
