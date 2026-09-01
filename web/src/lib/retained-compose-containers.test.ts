// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import {
  currentRetainedContainers,
  retainedContainerLogsPath,
} from './retained-compose-containers'

describe('retained Compose container diagnostics', () => {
  test('shows only container rows that have not been replaced', () => {
    const containers = [
      { container_id: 'retained', is_current: true },
      { container_id: 'replaced', is_current: false },
    ]

    expect(currentRetainedContainers(containers)).toEqual([containers[0]])
    expect(currentRetainedContainers(undefined)).toEqual([])
  })

  test('keeps the failed deployment scope in the authenticated logs link', () => {
    expect(retainedContainerLogsPath('my-project', 7, 42, 'container-id')).toBe(
      '/projects/my-project/environments/containers/container-id?env=7&deployment=42'
    )
  })
})
