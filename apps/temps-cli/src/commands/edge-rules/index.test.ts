// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { Command } from 'commander'
import openapi from '../../../openapi.json'
import { moveInOrder, parseId, parseMode, registerEdgeRulesCommands } from './index.js'
import { TOOL_GROUPS } from '../mcp/groups.js'
import { runWithApi } from '../../lib/mock-api.js'
import { mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

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

describe('apply', () => {
  const blockAdmin = {
    combinator: 'all',
    children: [{ kind: 'condition', field: 'path', op: 'starts_with', values: ['/admin'] }],
  }
  const savedRule = {
    id: 7,
    project_id: 1,
    environment_id: null,
    position: 0,
    name: 'Old rule',
    enabled: true,
    mode: 'enforce',
    expression: blockAdmin,
    action: { type: 'block', status: 403 },
    revision: 1,
    summary: 'Block with 403 when path starts with /admin',
    hits: 0,
    last_hit_at: null,
    created_at: '2026-10-31T00:00:00Z',
    updated_at: '2026-10-31T00:00:00Z',
  }

  async function apply(rules: unknown[], routes: Record<string, unknown>) {
    const dir = mkdtempSync(join(tmpdir(), 'edge-rules-apply-'))
    const file = join(dir, 'rules.json')
    await Bun.write(file, JSON.stringify({ rules }))
    try {
      return await runWithApi(
        registerEdgeRulesCommands,
        ['edge-rules', 'apply', '--project-id', '1', '--file', file, '--yes'],
        {
          'GET /projects/1/edge-rules': { project_id: 1, rules: [savedRule], max_rules: 100 },
          ...routes,
        },
      )
    } finally {
      rmSync(dir, { recursive: true, force: true })
    }
  }

  const writes = (requests: string[]) => requests.filter((r) => !r.startsWith('GET ') && !r.endsWith('/evaluate'))

  test('a rule the server would refuse deletes nothing', async () => {
    const run = await apply([{ name: 'New rule', expression: blockAdmin, action: { type: 'unknown' } }], {
      'POST /projects/1/edge-rules/evaluate': new Response(
        'Failed to deserialize the JSON body into the target type: rules[0].action: unknown variant `unknown`',
        { status: 422 },
      ),
    })
    expect(run.error?.message ?? run.stderr).toContain('nothing changed')
    expect(writes(run.requests)).toEqual([])
  })

  test('an environment the project does not have deletes nothing', async () => {
    const run = await apply(
      [{ name: 'New rule', environment_id: 9, expression: blockAdmin, action: { type: 'block', status: 403 } }],
      {
        'POST /projects/1/edge-rules/evaluate': { decision: { kind: 'forward' }, summary: '', trace: [] },
        'GET /projects/1/environments': [{ id: 5, name: 'production' }],
      },
    )
    expect(run.error?.message ?? run.stderr).toContain('has no environment 9')
    expect(writes(run.requests)).toEqual([])
  })

  test('a valid file is checked whole, then deleted, created and ordered', async () => {
    const run = await apply([{ name: 'New rule', expression: blockAdmin, action: { type: 'block', status: 403 } }], {
      'POST /projects/1/edge-rules/evaluate': { decision: { kind: 'forward' }, summary: '', trace: [] },
      'DELETE /projects/1/edge-rules/7': {},
      'POST /projects/1/edge-rules': { ...savedRule, id: 8, name: 'New rule' },
      'PUT /projects/1/edge-rules/order': { project_id: 1, rules: [], max_rules: 100 },
    })
    expect(run.error).toBeUndefined()
    expect(run.requests.filter((r) => !r.startsWith('GET '))).toEqual([
      'POST /projects/1/edge-rules/evaluate',
      'DELETE /projects/1/edge-rules/7',
      'POST /projects/1/edge-rules',
      'PUT /projects/1/edge-rules/order',
    ])
    const checked = run.bodies.find((b) => b.request.endsWith('/evaluate'))?.body as { rules: unknown[] }
    expect(checked.rules).toHaveLength(1)
  })
})
