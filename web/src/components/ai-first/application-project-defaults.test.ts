// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { APPLICATION_PROJECT_DEFAULTS } from './application-project-defaults'

describe('application project defaults', () => {
  test('uses Autopack for newly created workspace projects', () => {
    expect(APPLICATION_PROJECT_DEFAULTS).toEqual({
      preset: 'autopack',
      exposed_port: 3000,
    })
  })
})
