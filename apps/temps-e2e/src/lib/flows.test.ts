// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { isTerminalDeployStatus } from './flows.ts'

describe('isTerminalDeployStatus', () => {
  test('keeps running deployments non-terminal until readiness completes', () => {
    expect(isTerminalDeployStatus('running')).toEqual({ terminal: false, ok: false })
  })

  test('recognizes completed and failed terminal states', () => {
    expect(isTerminalDeployStatus('completed')).toEqual({ terminal: true, ok: true })
    expect(isTerminalDeployStatus('failed')).toEqual({ terminal: true, ok: false })
  })
})
