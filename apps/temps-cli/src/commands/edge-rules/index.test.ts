// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { Command } from 'commander'
import openapi from '../../../openapi.json'
import { moveInOrder, parseId, parseMode, registerEdgeRulesCommands } from './index.js'
import { TOOL_GROUPS } from '../mcp/groups.js'

function program(): Command {
  const p = new Command()
  registerEdgeRulesCommands(p)
  return p
}

describe('edge-rules command', () => {
  test('covers every edge rules operation in the API', () => {
    const edgeRules = program().commands.find((c) => c.name() === 'edge-rules')
    const names = edgeRules?.commands.map((c) => c.name()) ?? []
    // Operation id -> the command that calls it.
    const coverage: Record<string, string> = {
      list_edge_rules: 'list',
      get_edge_rule: 'get',
      create_edge_rule: 'create',
      update_edge_rule: 'update',
      delete_edge_rule: 'delete',
      reorder_edge_rules: 'move',
      evaluate_edge_rules: 'test',
    }
    const paths = openapi.paths as Record<string, Record<string, { operationId?: string }>>
    const operations = Object.entries(paths)
      .filter(([path]) => path.includes('/edge-rules'))
      .flatMap(([, methods]) => Object.values(methods).map((op) => op.operationId ?? ''))
    expect(operations.length).toBeGreaterThan(0)
    for (const operation of operations) {
      const command = coverage[operation]
      expect(command, `no CLI command for ${operation}`).toBeDefined()
      expect(names).toContain(command ?? '')
    }
    for (const extra of ['enable', 'disable', 'enforce', 'log-only', 'export', 'apply']) {
      expect(names).toContain(extra)
    }
  })

  test('is listed in the networking MCP group', () => {
    const networking = TOOL_GROUPS.find((g) => g.key === 'networking')
    expect(networking?.categories).toContain('edge-rules')
  })

  test('--mode has no commander default, so an explicit log-only can override a file', () => {
    const create = program()
      .commands.find((c) => c.name() === 'edge-rules')
      ?.commands.find((c) => c.name() === 'create')
    const mode = create?.options.find((o) => o.long === '--mode')
    expect(mode).toBeDefined()
    expect(mode?.defaultValue).toBeUndefined()
  })
})

describe('parsers', () => {
  test('parseMode accepts both spellings of log-only', () => {
    expect(parseMode('enforce')).toBe('enforce')
    expect(parseMode('log-only')).toBe('log_only')
    expect(parseMode('log_only')).toBe('log_only')
    expect(() => parseMode('block')).toThrow('--mode')
  })

  test('parseId names the flag', () => {
    expect(parseId('3', '--to')).toBe(3)
    expect(() => parseId('0', '--to')).toThrow('--to')
    expect(() => parseId('x', 'rule id')).toThrow('rule id')
  })

  test('moveInOrder moves to a 1-based position and clamps', () => {
    expect(moveInOrder([1, 2, 3], 3, 1)).toEqual([3, 1, 2])
    expect(moveInOrder([1, 2, 3], 1, 3)).toEqual([2, 3, 1])
    expect(moveInOrder([1, 2, 3], 2, 99)).toEqual([1, 3, 2])
    expect(() => moveInOrder([1, 2], 9, 1)).toThrow('Edge rule 9')
  })
})
