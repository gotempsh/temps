// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { containerWorkload } from './container-workload'

test('retained history without port metadata is unknown, including Compose HTTP containers', () => {
  expect(containerWorkload({})).toBe('unknown')
  expect(containerWorkload(undefined)).toBe('unknown')
  expect(containerWorkload({ container_port: 8080 })).toBe('service')
  expect(containerWorkload({ container_port: null })).toBe('worker')
})
