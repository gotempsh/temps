// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  computeCoverage,
  coverageDrift,
  describeCoverageDrift,
  directClientCalls,
  formatAllowlist,
  listOperations,
  normalizePath,
  rawFetchCalls,
  readAllowlist,
  repositoryCoverage,
  schemaProperties,
  sdkFunctionsByOperation,
  type Allowlist,
  type CommandAreas,
} from './api-coverage'

const spec = {
  paths: {
    '/widgets': {
      get: {
        operationId: 'list_widgets',
        parameters: [
          { in: 'query', name: 'page' },
          { in: 'header', name: 'x-trace' },
        ],
      },
      post: {
        operationId: 'create_widget',
        requestBody: { content: { 'application/json': { schema: { $ref: '#/components/schemas/CreateWidget' } } } },
      },
    },
    '/widgets/{id}': {
      parameters: [{ in: 'query', name: 'expand' }],
      delete: { operationId: 'delete_widget' },
    },
    '/gadgets/{gadget_id}/run': { post: { operationId: 'run_gadget' } },
  },
  components: {
    schemas: {
      CreateWidget: {
        allOf: [{ $ref: '#/components/schemas/Named' }],
        properties: { color: { type: 'string' }, id: { type: 'integer', readOnly: true } },
      },
      Named: { properties: { name: { type: 'string' } } },
    },
  },
}

const sdk = `
export const listWidgets = (options) => (options.client ?? client).get<A, B, C>({ url: '/widgets', ...options });
export const createWidget = (options) => (options.client ?? client).post<A, B, C>({
    url: '/widgets',
    ...options
});
export const deleteWidget = (options) => (options.client ?? client).delete<A, B, C>({ url: '/widgets/{id}', ...options });
export const runGadget = (options) => (options.client ?? client).sse.post<A, B, C>({ url: '/gadgets/{gadget_id}/run', ...options });
`

describe('listOperations', () => {
  test('collects query parameters, including path-level ones, and request-body properties', () => {
    const operations = listOperations(spec)
    expect(operations.map((op) => op.key)).toEqual([
      'DELETE /widgets/{id}',
      'GET /widgets',
      'POST /gadgets/{gadget_id}/run',
      'POST /widgets',
    ])
    expect(operations.find((op) => op.key === 'GET /widgets')?.fields).toEqual(['query.page'])
    expect(operations.find((op) => op.key === 'DELETE /widgets/{id}')?.fields).toEqual(['query.expand'])
    // allOf is followed through $ref; readOnly properties are not inputs.
    expect(operations.find((op) => op.key === 'POST /widgets')?.fields).toEqual(['body.color', 'body.name'])
  })

  test('schemaProperties survives a self-referencing schema', () => {
    const components = { Node: { properties: { child: { $ref: '#/components/schemas/Node' } }, allOf: [{ $ref: '#/components/schemas/Node' }] } }
    expect(schemaProperties({ $ref: '#/components/schemas/Node' }, components)).toEqual(['child'])
  })

  test('oneOf variants all count as inputs', () => {
    const components = {}
    const schema = { oneOf: [{ properties: { a: {} } }, { properties: { b: {} } }] }
    expect(schemaProperties(schema, components).sort()).toEqual(['a', 'b'])
  })
})

describe('call detection', () => {
  test('normalizePath ignores parameter names, template expressions and a missing leading slash', () => {
    expect(normalizePath('projects/{project_id}/x')).toBe('/projects/{}/x')
    expect(normalizePath('/projects/${id}/x')).toBe('/projects/{}/x')
  })

  test('sdkFunctionsByOperation maps SDK exports, SSE included, to their method and path', () => {
    const map = sdkFunctionsByOperation(sdk)
    expect(map.get('GET /widgets')).toEqual(['listWidgets'])
    expect(map.get('POST /widgets')).toEqual(['createWidget'])
    expect(map.get('POST /gadgets/{}/run')).toEqual(['runGadget'])
  })

  test('directClientCalls reads client.<method>({ url }) calls', () => {
    const calls = directClientCalls("await client.patch<Resp>({ url: 'widgets/{id}', body })\nclient.get({ url: `/x/${a}` })")
    expect([...calls].sort()).toEqual(['GET /x/{}', 'PATCH /widgets/{}'])
  })

  test('rawFetchCalls reads markers and fetch URLs, and skips literals whose method is unknown', () => {
    const paths = new Set(['/widgets', '/widgets/{}', '/gadgets/{}/run'])
    const source = [
      '// api-coverage: DELETE /widgets/{id}',
      'const response = await fetch(`${base}/widgets?page=2`)',
      "await fetch(`${base}/gadgets/${id}/run`, { method: 'POST' })",
      "const help = '/widgets/{id}'",
    ].join('\n')
    expect([...rawFetchCalls(source, paths)].sort()).toEqual(['DELETE /widgets/{}', 'GET /widgets', 'POST /gadgets/{}/run'])
  })

  test('without a fetch call, only markers count', () => {
    expect([...rawFetchCalls("const url = `${base}/widgets`", new Set(['/widgets']))]).toEqual([])
  })
})

function areas(entries: Record<string, string>): CommandAreas {
  return new Map(Object.entries(entries))
}

describe('computeCoverage', () => {
  test('reports uncalled operations, unsettable fields and reads without --json', () => {
    const report = computeCoverage(
      spec,
      sdk,
      areas({
        widgets: "listWidgets({ query: { page } })\ncreateWidget({ body: { name } })\n.option('--json')",
        gadgets: 'runGadget({})',
      }),
    )
    expect(report.covered).toEqual(['GET /widgets', 'POST /gadgets/{gadget_id}/run', 'POST /widgets'])
    expect(report.gaps).toEqual(['DELETE /widgets/{id}', 'POST /widgets body.color'])
    expect(report.fieldsCovered).toBe(2)
    expect(report.fieldsTotal).toBe(3)
  })

  test('a read whose area has no --json is a gap', () => {
    const report = computeCoverage(spec, sdk, areas({ widgets: 'listWidgets({ query: { page } })' }))
    expect(report.gaps).toContain('GET /widgets --json')
  })

  test('an area that registers through endpoint() offers --json', () => {
    const source = "import { endpoint } from '../../lib/endpoint.js'\nendpoint(group, listWidgets, { query: { page: 'int' } })"
    const report = computeCoverage(spec, sdk, areas({ widgets: source }))
    expect(report.gaps).not.toContain('GET /widgets --json')
  })

  test('an SDK name only counts as a whole identifier', () => {
    const report = computeCoverage(spec, sdk, areas({ widgets: 'listWidgetsFast()' }))
    expect(report.covered).toEqual([])
  })

  test('a marker naming no operation is reported', () => {
    const report = computeCoverage(spec, sdk, areas({ widgets: '// api-coverage: GET /nothing/here' }))
    expect(report.unknownMarkers).toEqual(['widgets: GET /nothing/here'])
    expect(describeCoverageDrift(coverageDrift(report, { excluded: [], pending: report.gaps }))).toContain(
      'GET /nothing/here',
    )
  })
})

describe('coverageDrift', () => {
  const report = computeCoverage(spec, sdk, areas({ widgets: "createWidget({ body: { name } })\n'--json'" }))
  // gaps: DELETE /widgets/{id}, GET /widgets, POST /gadgets/{gadget_id}/run, POST /widgets body.color

  test('a gap in neither list fails, naming the allowlist', () => {
    const drift = coverageDrift(report, { excluded: [], pending: ['GET /widgets'] })
    expect(drift.uncovered).toEqual(['DELETE /widgets/{id}', 'POST /gadgets/{gadget_id}/run', 'POST /widgets body.color'])
    expect(describeCoverageDrift(drift)).toContain('cli-api-coverage.json')
  })

  test('nothing drifts when every gap is listed', () => {
    const allowlist: Allowlist = {
      excluded: [{ reason: 'Called by gadgets, not people', entries: ['POST /gadgets/{gadget_id}/run'] }],
      pending: ['DELETE /widgets/{id}', 'GET /widgets', 'POST /widgets body.color'],
    }
    expect(describeCoverageDrift(coverageDrift(report, allowlist))).toBeNull()
  })

  test('pending may only shrink: a covered or removed entry is stale', () => {
    const drift = coverageDrift(report, {
      excluded: [],
      pending: [...report.gaps, 'POST /widgets body.name', 'GET /removed'],
    })
    expect(drift.stalePending).toEqual(['GET /removed', 'POST /widgets body.name'])
    expect(describeCoverageDrift(drift)).toContain('remove them')
  })

  test('an excluded operation also covers its own field gaps', () => {
    const drift = coverageDrift(report, {
      excluded: [{ reason: 'Not for the CLI', entries: ['POST /widgets body.color'] }],
      pending: ['DELETE /widgets/{id}', 'GET /widgets', 'POST /gadgets/{gadget_id}/run'],
    })
    expect(drift.uncovered).toEqual([])
  })

  test('every exclusion group needs a reason, and an entry is listed once', () => {
    const drift = coverageDrift(report, {
      excluded: [{ reason: ' ', entries: ['GET /widgets'] }],
      pending: ['GET /widgets', 'DELETE /widgets/{id}', 'POST /gadgets/{gadget_id}/run', 'POST /widgets body.color'],
    })
    expect(drift.unexplained).toBe(1)
    expect(drift.duplicates).toEqual(['GET /widgets'])
    expect(describeCoverageDrift(drift)).toContain('no reason')
  })
})

describe('repository', () => {
  test('every API operation and input is covered by a command, excluded with a reason, or pending', () => {
    const failure = describeCoverageDrift(coverageDrift(repositoryCoverage(), readAllowlist()))
    expect(failure).toBeNull()
  })

  test('cli-api-coverage.json is in canonical form', async () => {
    const path = new URL('../cli-api-coverage.json', import.meta.url).pathname
    const text = await Bun.file(path).text()
    expect(text).toBe(formatAllowlist(readAllowlist(path)))
  })
})
