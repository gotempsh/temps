// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  isSandboxCreateTool,
  isSandboxScopedTool,
  isSandboxOfferTool,
  linkableSandboxes,
  offerReason,
  sandboxButtonLabel,
} from './chat-sandbox'

describe('isSandboxOfferTool', () => {
  test('matches the tool by name, with or without an MCP prefix', () => {
    expect(isSandboxOfferTool('temps_offer_sandbox')).toBe(true)
    expect(isSandboxOfferTool('mcp__temps-chat__temps_offer_sandbox')).toBe(
      true
    )
    expect(isSandboxOfferTool('temps_write')).toBe(false)
    expect(isSandboxOfferTool('sandbox_exec')).toBe(false)
  })
})

describe('isSandboxCreateTool', () => {
  test('matches the create tool only, with or without an MCP prefix', () => {
    expect(isSandboxCreateTool('temps_create_sandbox')).toBe(true)
    expect(isSandboxCreateTool('mcp__temps-chat__temps_create_sandbox')).toBe(
      true
    )
    expect(isSandboxCreateTool('temps_offer_sandbox')).toBe(false)
    expect(isSandboxCreateTool('sandbox_exec')).toBe(false)
  })
})

describe('linkableSandboxes', () => {
  test('drops destroyed sandboxes, agent runs and application workspaces', () => {
    const sandboxes = [
      { id: 'sbx_1', name: 'scratch', status: 'running' },
      { id: 'sbx_2', name: 'old', status: 'destroyed' },
      { id: 'sbx_3', name: 'run', status: 'running', agent_run_id: 9 },
      { id: 'sbx_4', name: 'ai-application:global-user-1', status: 'running' },
      { id: 'sbx_5', name: 'paused', status: 'stopped', agent_run_id: null },
    ]
    expect(linkableSandboxes(sandboxes).map((s) => s.id)).toEqual([
      'sbx_1',
      'sbx_5',
    ])
  })
})

describe('sandboxButtonLabel', () => {
  test('names the linked sandbox, otherwise just the feature', () => {
    expect(sandboxButtonLabel(undefined)).toBe('Sandbox')
    expect(sandboxButtonLabel({ configured: true, linked: null })).toBe(
      'Sandbox'
    )
    expect(
      sandboxButtonLabel({ configured: true, linked: { name: 'scratch' } })
    ).toBe('scratch')
  })
})

describe('offerReason', () => {
  test('reads the reason and ends it with one full stop', () => {
    expect(offerReason('{"reason":"unpack the archive. "}')).toBe(
      'unpack the archive.'
    )
  })
  test('is null for missing, empty or malformed arguments', () => {
    expect(offerReason('{}')).toBeNull()
    expect(offerReason('{"reason":"  "}')).toBeNull()
    expect(offerReason('{"reason":4}')).toBeNull()
    expect(offerReason('not json')).toBeNull()
    expect(offerReason('')).toBeNull()
  })
})

describe('isSandboxScopedTool', () => {
  test('matches the tools that run in the linked sandbox, bare or prefixed', () => {
    expect(isSandboxScopedTool('sandbox_exec')).toBe(true)
    expect(isSandboxScopedTool('temps__sandbox_read_file')).toBe(true)
    expect(isSandboxScopedTool('sandbox_write_file')).toBe(true)
    expect(isSandboxScopedTool('sandbox_preview')).toBe(true)
  })

  test('does not match other tools', () => {
    expect(isSandboxScopedTool('temps_create_sandbox')).toBe(false)
    expect(isSandboxScopedTool('temps')).toBe(false)
  })
})
