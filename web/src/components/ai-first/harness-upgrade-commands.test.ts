// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import {
  harnessUpgradeCommands,
  sandboxShellCommand,
} from './harness-upgrade-commands'

describe('harnessUpgradeCommands', () => {
  test('returns provider-native upgrades followed by version checks', () => {
    const commands = harnessUpgradeCommands('sbx_0123456789abcdef')

    expect(commands.map(({ providerId }) => providerId)).toEqual([
      'claude_cli',
      'codex_cli',
      'opencode',
    ])
    expect(commands[0].command).toBe('claude update && claude --version')
    expect(commands[1].command).toContain('@openai/codex@latest')
    expect(commands[1].command).toContain('codex --version')
    expect(commands[2].command).toBe(
      'opencode upgrade --method curl && opencode --version'
    )
  })

  test('builds a local CLI command without expanding sandbox HOME locally', () => {
    const codex = harnessUpgradeCommands('sbx_0123456789abcdef')[1]

    expect(codex.cliCommand).toBe(
      `temps sandbox exec sbx_0123456789abcdef -- sh -lc 'BUN_INSTALL="$HOME/.bun" BUN_INSTALL_BIN="$HOME/.bun/bin" bun add -g @openai/codex@latest && codex --version'`
    )
  })

  test('builds the existing reattachable CLI shell command', () => {
    expect(sandboxShellCommand('sbx_0123456789abcdef')).toBe(
      'temps sandbox shell sbx_0123456789abcdef'
    )
  })

  test('rejects IDs outside the backend public-ID contract', () => {
    for (const id of [
      'sbx_ok; touch /tmp/example',
      'sbx_example',
      'sbx_0123456789abcdef0',
    ]) {
      expect(() => harnessUpgradeCommands(id)).toThrow(
        'Invalid sandbox public ID'
      )
      expect(() => sandboxShellCommand(id)).toThrow('Invalid sandbox public ID')
    }
  })
})
