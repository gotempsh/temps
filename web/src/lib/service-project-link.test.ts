// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  serviceCreateHref,
  serviceProjectId,
  serviceProjectLink,
} from './service-project-link'

describe('service project linking', () => {
  test('keeps a valid project through provider selection and service creation', () => {
    expect(serviceProjectId('42')).toBe(42)
    expect(serviceCreateHref('postgres', 42)).toBe(
      '/storage/create?type=postgres&project_id=42'
    )
    expect(serviceProjectLink(42)).toEqual({ project_id: 42 })
  })

  test('does not invent a project link without a valid project id', () => {
    expect(serviceProjectId(null)).toBeNull()
    expect(serviceProjectId('invalid')).toBeNull()
    expect(serviceProjectLink(null)).toEqual({})
  })
})
