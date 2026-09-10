// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { AI_CLI_PROVIDER_SHELL } from './ai-cli-providers'

describe('AI CLI provider shell', () => {
  test('uses the backend catalog identifiers', () => {
    expect(AI_CLI_PROVIDER_SHELL.map((provider) => provider.id)).toEqual([
      'claude_cli',
      'codex_cli',
      'opencode',
    ])
  })
})
