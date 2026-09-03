// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  SANDBOX_CLI_EXAMPLE,
  SANDBOX_WORKSPACE_EXAMPLE,
} from './CreateSandboxDocs'

describe('sandbox CLI onboarding', () => {
  test('uses the real context store and targets the chosen instance explicitly', () => {
    expect(SANDBOX_CLI_EXAMPLE).toContain('~/.temps/.contexts.json')
    expect(SANDBOX_CLI_EXAMPLE).toContain(
      'login https://your-temps-instance.com --context my-instance'
    )
    expect(SANDBOX_CLI_EXAMPLE).toContain('--target-context my-instance')
    expect(SANDBOX_CLI_EXAMPLE).not.toContain('~/.config/temps/auth.json')

    for (const command of SANDBOX_WORKSPACE_EXAMPLE.split('\n').filter((line) =>
      line.startsWith('bunx ')
    )) {
      expect(command).toContain('--target-context my-instance')
    }
  })
})
