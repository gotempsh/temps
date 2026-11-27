// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { AiHarnessCredential } from '@/api/client'
import {
  CONNECTABLE_AGENTS,
  agentConnection,
  mcpClientFor,
  agentWaitingLine,
  mcpSetupBlocker,
} from './agents'

describe('agentWaitingLine', () => {
  test('no key yet is not a problem to fix: the login creates it', () => {
    expect(
      agentWaitingLine(
        'missing',
        "No 'Temps AI harness' API key exists yet; create one to connect a harness"
      )
    ).toBe('Waiting for your agent to log in…')
    expect(agentWaitingLine(undefined, null)).toBe(
      'Waiting for your agent to log in…'
    )
  })

  test('a key that exists but is unused says why', () => {
    expect(agentWaitingLine('waiting', 'The key has not been used yet')).toBe(
      'The key has not been used yet'
    )
  })
})

test('the agents shown are the ones with a logo', () => {
  expect(CONNECTABLE_AGENTS.map((a) => a.name)).toEqual([
    'Claude Code',
    'Codex',
    'Cursor',
    'GitHub Copilot',
    'OpenCode',
    'Windsurf',
  ])
})

describe('agentConnection', () => {
  const via = (
    kind: AiHarnessCredential['kind'],
    name = 'mcp-claude-app-2026-11-25T10-00-00Z'
  ): AiHarnessCredential => ({
    id: 3,
    name,
    key_prefix: 'tk_ab12c',
    kind,
    last_used_at: '2026-11-25T10:00:00Z',
  })

  test('a coding agent is connected by any agent credential', () => {
    expect(
      agentConnection('coding_agent', {
        status: 'connected',
        connected_via: via('cli_login'),
      })
    ).toEqual({ kind: 'connected' })
  })

  test('the Claude app is connected only once its own key is used', () => {
    expect(
      agentConnection(
        'claude_app',
        { status: 'connected', connected_via: via('mcp_client') },
        mcpClientFor('claude_app', 'cursor')
      )
    ).toEqual({ kind: 'connected' })
  })

  test("another app's MCP key does not connect the Claude app, or a different app", () => {
    const cursor = {
      status: 'connected' as const,
      connected_via: via('mcp_client', 'mcp-cursor-2026-11-20T09-00-00Z'),
    }
    const claude = agentConnection(
      'claude_app',
      cursor,
      mcpClientFor('claude_app', 'cursor')
    )
    expect(claude).toEqual({
      kind: 'waiting',
      line: 'Waiting for the Claude app to call Temps. Another agent is already connected, so you can continue without it.',
      canContinue: true,
    })
    expect(
      agentConnection('other_app', cursor, mcpClientFor('other_app', 'zed'))
        .kind
    ).toBe('waiting')
    expect(
      agentConnection('other_app', cursor, mcpClientFor('other_app', 'cursor'))
    ).toEqual({ kind: 'connected' })
  })

  test('an earlier CLI login does not count for the Claude app, but lets you continue', () => {
    const shown = agentConnection('claude_app', {
      status: 'connected',
      connected_via: via('cli_login'),
    })
    expect(shown).toEqual({
      kind: 'waiting',
      line: 'Waiting for the Claude app to call Temps. Another agent is already connected, so you can continue without it.',
      canContinue: true,
    })
  })

  test('another app waits for an MCP key in the same way', () => {
    expect(
      agentConnection(
        'other_app',
        {
          status: 'connected',
          connected_via: via('mcp_client', 'mcp-vscode-2026-11-25T10-00-00Z'),
        },
        mcpClientFor('other_app', 'vscode')
      )
    ).toEqual({ kind: 'connected' })
    expect(agentConnection('other_app', { status: 'missing' })).toEqual({
      kind: 'waiting',
      line: 'Waiting for your app to call Temps…',
      canContinue: false,
    })
  })

  test('nothing connected yet waits, and Continue stays off', () => {
    expect(agentConnection('claude_app', { status: 'missing' })).toEqual({
      kind: 'waiting',
      line: 'Waiting for the Claude app to call Temps…',
      canContinue: false,
    })
    expect(agentConnection('coding_agent', undefined)).toEqual({
      kind: 'waiting',
      line: 'Waiting for your agent to log in…',
      canContinue: false,
    })
  })
})

describe('mcpSetupBlocker', () => {
  const next = 'download the extension'

  test('the last step needs the MCP server on and a key', () => {
    expect(mcpSetupBlocker('off', false, next)).toBe(
      'First turn on the MCP server and create a key, then download the extension.'
    )
    expect(mcpSetupBlocker('off', true, next)).toBe(
      'First turn on the MCP server, then download the extension.'
    )
    expect(mcpSetupBlocker('on', false, 'copy the settings')).toBe(
      'First create a key, then copy the settings.'
    )
    expect(mcpSetupBlocker('on', true, next)).toBeNull()
  })

  test('while the state loads it waits, and an unreadable state gates on the key only', () => {
    expect(mcpSetupBlocker('loading', true, next)).toBe(
      'Checking whether the MCP server is on…'
    )
    expect(mcpSetupBlocker('unknown', false, next)).toBe(
      'First create a key, then download the extension.'
    )
    expect(mcpSetupBlocker('unknown', true, next)).toBeNull()
  })
})
