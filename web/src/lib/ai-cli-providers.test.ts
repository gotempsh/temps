// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  AI_CLI_PROVIDER_SHELL,
  isWorkspaceChatOnlyProvider,
  projectAgentProviders,
} from './ai-cli-providers'

describe('AI CLI provider shell', () => {
  test('uses the backend catalog identifiers', () => {
    expect(AI_CLI_PROVIDER_SHELL.map((provider) => provider.id)).toEqual([
      'claude_cli',
      'codex_cli',
      'opencode',
      'pi',
    ])
  })
})

describe('isWorkspaceChatOnlyProvider', () => {
  test('limits pi to workspace chat', () => {
    expect(isWorkspaceChatOnlyProvider('pi')).toBe(true)
  })

  test('keeps host-capable harnesses and look-alike ids available', () => {
    for (const id of [
      'claude_cli',
      'codex_cli',
      'opencode',
      'api',
      'pipeline',
      'PI',
      ' pi',
    ]) {
      expect(isWorkspaceChatOnlyProvider(id)).toBe(false)
    }
  })
})

describe('projectAgentProviders', () => {
  test('drops workspace-only harnesses and keeps catalog order', () => {
    const providers = AI_CLI_PROVIDER_SHELL.map(({ id }) => ({
      id,
      credential_saved: true,
    }))
    expect(projectAgentProviders(providers).map(({ id }) => id)).toEqual([
      'claude_cli',
      'codex_cli',
      'opencode',
    ])
  })

  test('a saved pi key alone does not make autofix ready', () => {
    const providers = [
      { id: 'claude_cli', credential_saved: false },
      { id: 'pi', credential_saved: true },
    ]
    expect(
      projectAgentProviders(providers).some(
        (provider) => provider.credential_saved
      )
    ).toBe(false)
  })
})
