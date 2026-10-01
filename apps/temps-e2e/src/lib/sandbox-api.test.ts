// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  SANDBOX_PROBLEM_TYPES,
  describeResult,
  evictionIssues,
  expectProblem,
  expectStatus,
  findPlacementNode,
  fromBase64,
  isNewerTimestamp,
  parseDockerNames,
  parseMarkerAndHostname,
  parseProblem,
  sandboxApiUrl,
  sandboxContainerName,
  sandboxContainersIn,
  sandboxImageFallbacks,
  sandboxLabel,
  toBase64,
  workerWorkDirCandidates,
  type ApiResult,
  type NodeEvictionResponse,
  type SandboxPlacement,
} from './sandbox-api.ts'

const problem = (status: number, body: Record<string, unknown>): ApiResult<unknown> => ({
  status,
  json: body,
  text: JSON.stringify(body),
})

describe('sandbox naming', () => {
  test('container label drops the public-id prefix, matching the worker naming', () => {
    expect(sandboxLabel('sbx_0b507bb997ea14d0')).toBe('0b507bb997ea14d0')
    expect(sandboxContainerName('sbx_0b507bb997ea14d0')).toBe('temps-sandbox-0b507bb997ea14d0')
  })

  test('an id without the prefix is used as-is', () => {
    expect(sandboxContainerName('abc')).toBe('temps-sandbox-abc')
  })

  test('work dir candidates follow <agent data dir>/sandboxes/<label>', () => {
    expect(workerWorkDirCandidates('sbx_ab12')).toEqual([
      '/root/.temps/sandboxes/ab12',
      '/var/lib/temps/sandboxes/ab12',
    ])
  })

  test('API URL is <base>/api/v1/sandboxes<path>', () => {
    expect(sandboxApiUrl('http://localhost:18180', '/placement')).toBe('http://localhost:18180/api/v1/sandboxes/placement')
    expect(sandboxApiUrl('http://localhost:18180/api/', '')).toBe('http://localhost:18180/api/v1/sandboxes')
  })
})

describe('docker listings', () => {
  const names = parseDockerNames(
    'temps-sandbox-aa11\n  temps-sandbox-egress-proxy-v2-temps-sandbox-aa11 \n\ntemps-sandbox-bb22\nmy-app-web-1\n',
  )

  test('parses one name per line, trimming blanks', () => {
    expect(names).toEqual([
      'temps-sandbox-aa11',
      'temps-sandbox-egress-proxy-v2-temps-sandbox-aa11',
      'temps-sandbox-bb22',
      'my-app-web-1',
    ])
  })

  test('every sandbox-owned container, application containers excluded', () => {
    expect(sandboxContainersIn(names)).toEqual([
      'temps-sandbox-aa11',
      'temps-sandbox-egress-proxy-v2-temps-sandbox-aa11',
      'temps-sandbox-bb22',
    ])
  })

  test("one sandbox's container and its egress proxy, not a sibling", () => {
    expect(sandboxContainersIn(names, 'sbx_aa11')).toEqual([
      'temps-sandbox-aa11',
      'temps-sandbox-egress-proxy-v2-temps-sandbox-aa11',
    ])
    expect(sandboxContainersIn(names, 'sbx_aa1')).toEqual([])
  })
})

describe('problem responses', () => {
  const notAllowed = problem(422, {
    type: SANDBOX_PROBLEM_TYPES.nodeNotAllowed,
    title: 'Sandbox Node Not Allowed',
    status: 422,
    detail: "Node 'worker-1' is not allowed to run sandboxes.",
  })

  test('parses RFC 7807 bodies and ignores other JSON', () => {
    expect(parseProblem(notAllowed.json)?.type).toBe(SANDBOX_PROBLEM_TYPES.nodeNotAllowed)
    expect(parseProblem({ sandbox: { id: 'sbx_1' } })).toBeUndefined()
    expect(parseProblem(undefined)).toBeUndefined()
  })

  test('accepts the expected status, type and named node', () => {
    expect(
      expectProblem(notAllowed, { status: 422, types: [SANDBOX_PROBLEM_TYPES.nodeNotAllowed], detailIncludes: 'WORKER-1' }, 'create')
        .title,
    ).toBe('Sandbox Node Not Allowed')
  })

  test('accepts any of several types', () => {
    const unified = problem(422, { type: SANDBOX_PROBLEM_TYPES.unsupportedOnWorkerNode, title: 'x' })
    expect(() =>
      expectProblem(
        unified,
        { status: 422, types: [SANDBOX_PROBLEM_TYPES.snapshotOnWorkerNode, SANDBOX_PROBLEM_TYPES.unsupportedOnWorkerNode] },
        'snapshot',
      ),
    ).not.toThrow()
  })

  test('rejects a wrong status, a wrong type, or a detail that does not name the node', () => {
    expect(() => expectProblem(notAllowed, { status: 503 }, 'exec')).toThrow(/expected HTTP 503, got HTTP 422/)
    expect(() => expectProblem(notAllowed, { status: 422, types: [SANDBOX_PROBLEM_TYPES.nodeOffline] }, 'create')).toThrow(
      /sandbox-node-offline/,
    )
    expect(() => expectProblem(notAllowed, { status: 422, detailIncludes: 'worker-2' }, 'create')).toThrow(/mention 'worker-2'/)
  })

  test('rejects a status match without a problem body', () => {
    expect(() => expectProblem({ status: 503, json: undefined, text: 'Service Unavailable' }, { status: 503 }, 'exec')).toThrow(
      /without a problem\+json body/,
    )
  })

  test('expectStatus returns the body or explains what came back', () => {
    expect(expectStatus({ status: 200, json: { ok: 1 }, text: '{"ok":1}' }, 200, 'get')).toEqual({ ok: 1 })
    expect(() => expectStatus(notAllowed, 201, 'create')).toThrow(/create: expected HTTP 201, got HTTP 422: .*not allowed/)
    expect(describeResult({ status: 0, json: undefined, text: 'The operation timed out.' })).toBe(
      'no response (The operation timed out.)',
    )
  })
})

describe('placement and eviction', () => {
  const placement: SandboxPlacement = {
    allowed_node_ids: [2],
    nodes: [
      { id: 0, name: 'control-plane', is_control_plane: true, status: 'active', allowed: false, eligible: false, reason: 'not allowed', live_sandboxes: 0 },
      { id: 2, name: 'worker-1', is_control_plane: false, status: 'active', allowed: true, eligible: true, reason: null, live_sandboxes: 1 },
    ],
  }

  test('finds nodes by id, control plane included', () => {
    expect(findPlacementNode(placement, 2)?.name).toBe('worker-1')
    expect(findPlacementNode(placement, 0)?.is_control_plane).toBe(true)
    expect(findPlacementNode(placement, 9)).toBeUndefined()
  })

  const eviction = (overrides: Partial<NodeEvictionResponse>): NodeEvictionResponse => ({
    node: placement.nodes[1]!,
    destroyed: ['sbx_a', 'sbx_b'],
    containers_unconfirmed: [],
    ...overrides,
  })

  test('a clean eviction of every expected sandbox has no issues', () => {
    expect(evictionIssues(eviction({}), ['sbx_a'])).toEqual([])
  })

  test('reports missing ids and unconfirmed containers with their cleanup command', () => {
    const issues = evictionIssues(
      eviction({
        destroyed: ['sbx_b'],
        containers_unconfirmed: [
          { sandbox_id: 'sbx_b', reason: 'timed out', cleanup_command: 'docker ps -aq --filter name=temps-sandbox-b | xargs -r docker rm -f' },
        ],
      }),
      ['sbx_a'],
    )
    expect(issues).toHaveLength(2)
    expect(issues[0]).toContain('sbx_a missing')
    expect(issues[1]).toContain('xargs -r docker rm -f')
  })
})

describe('exec output and file contents', () => {
  test('finds the hostname on the line after the marker', () => {
    expect(parseMarkerAndHostname('temps-e2e-run1\na1b2c3d4e5f6\n', 'temps-e2e-run1')).toEqual({
      markerSeen: true,
      hostname: 'a1b2c3d4e5f6',
    })
    expect(parseMarkerAndHostname('something else\n', 'temps-e2e-run1')).toEqual({ markerSeen: false, hostname: '' })
  })

  test('base64 round-trips UTF-8 text exactly', () => {
    const text = 'written by run-1 ✓\nline two\n'
    expect(fromBase64(toBase64(text))).toBe(text)
  })
})

describe('heartbeat freshness', () => {
  test('a later heartbeat is newer, an equal or earlier one is not', () => {
    expect(isNewerTimestamp('2026-10-01T10:00:31Z', '2026-10-01T10:00:01Z')).toBe(true)
    expect(isNewerTimestamp('2026-10-01T10:00:01Z', '2026-10-01T10:00:01Z')).toBe(false)
    expect(isNewerTimestamp('2026-10-01T09:59:59.999Z', '2026-10-01T10:00:00Z')).toBe(false)
  })

  test('no previous heartbeat means any valid one is newer; a missing one never is', () => {
    expect(isNewerTimestamp('2026-10-01T10:00:00Z', '')).toBe(true)
    expect(isNewerTimestamp('', '2026-10-01T10:00:00Z')).toBe(false)
    expect(isNewerTimestamp('', '')).toBe(false)
  })
})

describe('sandboxImageFallbacks', () => {
  test('falls back to the beta tags of the same GHCR sandbox image', () => {
    expect(sandboxImageFallbacks('ghcr.io/gotempsh/temps-sandbox-node:0.1.0')).toEqual([
      'ghcr.io/gotempsh/temps-sandbox-node:0.1.0-beta',
      'ghcr.io/gotempsh/temps-sandbox-node:beta',
    ])
  })

  test('a beta tag only falls back to the floating beta tag', () => {
    expect(sandboxImageFallbacks('ghcr.io/gotempsh/temps-sandbox-node:0.1.0-beta')).toEqual([
      'ghcr.io/gotempsh/temps-sandbox-node:beta',
    ])
  })

  test('custom images, digests and untagged names get no fallback', () => {
    expect(sandboxImageFallbacks('registry.example.com/team/sandbox:1.0')).toEqual([])
    expect(sandboxImageFallbacks('ghcr.io/gotempsh/temps-sandbox-node@sha256:abc')).toEqual([])
    expect(sandboxImageFallbacks('ghcr.io/gotempsh/temps-sandbox-node')).toEqual([])
  })
})
