// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import {
  CLI_ROOT,
  callsFunction,
  codeOf,
  computeCoverage,
  coverageDrift,
  describeCoverageDrift,
  directClientCalls,
  formatAllowlist,
  listOperations,
  mentions,
  normalizePath,
  parentDirOf,
  rawFetchCalls,
  readAllowlist,
  repositoryCoverage,
  schemaProperties,
  sdkFunctionsByOperation,
  stripComments,
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

  test('double-quoted URLs count like single-quoted ones', () => {
    expect([...directClientCalls('client.get({ url: "/widgets" })')]).toEqual(['GET /widgets'])
    expect([...rawFetchCalls('await fetch("/widgets")', new Set(['/widgets']))]).toEqual(['GET /widgets'])
  })

  test('a trailing interpolation is a path parameter, or a query string when the path is shorter', () => {
    const paths = new Set(['/widgets', '/widgets/{}'])
    expect([...rawFetchCalls("await fetch(`${base}/widgets/${id}`, { method: 'DELETE' })", paths)]).toEqual(['DELETE /widgets/{}'])
    expect([...rawFetchCalls('await fetch(`${base}/widgets${qs}`)', paths)]).toEqual(['GET /widgets'])
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

  test('an SDK function that is only imported or named in a comment is not called', () => {
    const source = "import { listWidgets } from '../../api/sdk.gen.js'\n// listWidgets is used elsewhere\n'--json'"
    expect(computeCoverage(spec, sdk, areas({ widgets: source })).covered).toEqual([])
  })

  test('an SDK name in an inline comment is not a call', () => {
    const source = "import { listWidgets } from '../../api/sdk.gen.js'\nconst label = 'widgets' // listWidgets\nconst x = 1 /* listWidgets() */\n'--json'"
    expect(computeCoverage(spec, sdk, areas({ widgets: source })).covered).toEqual([])
  })

  test('a reference that is never called does not count; a call or an endpoint() registration does', () => {
    const referenced = "import { listWidgets } from '../../api/sdk.gen.js'\nconst handlers = { list: listWidgets }\nconst f = listWidgets\n'--json'"
    expect(computeCoverage(spec, sdk, areas({ widgets: referenced })).covered).toEqual([])
    expect(callsFunction('listWidgets({ query: {} })', 'listWidgets')).toBe(true)
    expect(callsFunction('listWidgets<true>({})', 'listWidgets')).toBe(true)
    expect(callsFunction('endpoint(group, listWidgets, { name: "list" })', 'listWidgets')).toBe(true)
    expect(callsFunction('other.listWidgets({})', 'listWidgets')).toBe(false)
    expect(callsFunction('api.listWidgets({})', 'listWidgets', ['api'])).toBe(true)
  })

  test('a namespace import of the SDK counts its qualified calls', () => {
    const source = "import * as api from '../../api/sdk.gen.js'\nawait api.listWidgets({ query: { page } })\n'--json'"
    expect(computeCoverage(spec, sdk, areas({ widgets: source })).covered).toEqual(['GET /widgets'])
  })

  test('stripComments keeps // inside strings and templates and blanks real comments', () => {
    expect(stripComments("const u = 'https://x.example' // listWidgets")).toBe("const u = 'https://x.example' ")
    expect(stripComments('const u = "a//b"; f()')).toBe('const u = "a//b"; f()')
    expect(stripComments('const t = `//${a /* c */ + "//"}//` // tail')).toBe('const t = `//${a         + "//"}//` ')
    expect(stripComments("a /* x\ny */ b")).toBe('a     \n     b')
    expect(stripComments("const q = 'it\\'s // here' // c")).toBe("const q = 'it\\'s // here' ")
  })

  test('an SDK function imported under another name counts where the local name is called', () => {
    const source = "import {\n  listWidgets as fetchWidgets,\n} from '../../api/sdk.gen.js'\nfetchWidgets({ query: { page } })\n'--json'"
    const report = computeCoverage(spec, sdk, areas({ widgets: source }))
    expect(report.covered).toEqual(['GET /widgets'])
    expect(codeOf(source).aliases.get('listWidgets')).toEqual(['fetchWidgets'])
  })

  test('an input name is matched literally', () => {
    expect(mentions('send({ "filter[name]": v })', 'filter[name]')).toBe(true)
    expect(mentions('send({ filtern: v })', 'filter[name]')).toBe(false)
    expect(mentions('a.bXc', 'a.b')).toBe(false)
    expect(mentions('page_size', 'page')).toBe(false)
  })

  test('the CLI root is the filesystem directory above scripts/', () => {
    expect(CLI_ROOT).toBe(`${resolve(import.meta.dir, '..')}/`)
  })

  test('parentDirOf decodes a URL path with a space and a literal %', () => {
    const url = 'file:///tmp/my%20check%25out/apps/temps-cli/scripts/api-coverage.ts'
    expect(parentDirOf(url)).toBe('/tmp/my check%out/apps/temps-cli/')
    expect(parentDirOf(new URL(url))).toBe('/tmp/my check%out/apps/temps-cli/')
  })

  test('a marker naming no operation is reported', () => {
    const report = computeCoverage(spec, sdk, areas({ widgets: '// api-coverage: GET /nothing/here' }))
    expect(report.unknownMarkers).toEqual(['widgets: GET /nothing/here'])
    expect(describeCoverageDrift(coverageDrift(report, { excluded: [] }))).toContain(
      'GET /nothing/here',
    )
  })
})

describe('coverageDrift', () => {
  const report = computeCoverage(spec, sdk, areas({ widgets: "createWidget({ body: { name } })\n'--json'" }))
  // gaps: DELETE /widgets/{id}, GET /widgets, POST /gadgets/{gadget_id}/run, POST /widgets body.color

  const others = ['DELETE /widgets/{id}', 'GET /widgets', 'POST /widgets body.color']

  test('a gap that is not excluded fails, naming the allowlist', () => {
    const drift = coverageDrift(report, { excluded: [{ reason: 'Not for people', entries: ['GET /widgets'] }] })
    expect(drift.uncovered).toEqual(['DELETE /widgets/{id}', 'POST /gadgets/{gadget_id}/run', 'POST /widgets body.color'])
    expect(describeCoverageDrift(drift)).toContain('cli-api-coverage.json')
  })

  test('nothing drifts when every gap is excluded with a reason', () => {
    const allowlist: Allowlist = {
      excluded: [
        { reason: 'Called by gadgets, not people', entries: ['POST /gadgets/{gadget_id}/run'] },
        { reason: 'Not for the CLI', entries: others },
      ],
    }
    expect(describeCoverageDrift(coverageDrift(report, allowlist))).toBeNull()
  })

  test('the pending list is closed: any entry in it fails, even a real gap', () => {
    const drift = coverageDrift(report, {
      excluded: [{ reason: 'Not for the CLI', entries: ['POST /gadgets/{gadget_id}/run', ...others.slice(1)] }],
      pending: ['DELETE /widgets/{id}'],
    })
    expect(drift.pending).toEqual(['DELETE /widgets/{id}'])
    expect(describeCoverageDrift(drift)).toContain('a closed list')
  })

  test('an excluded operation also covers its own field gaps', () => {
    const drift = coverageDrift(report, {
      excluded: [{ reason: 'Not for the CLI', entries: ['POST /widgets body.color', 'DELETE /widgets/{id}', 'GET /widgets', 'POST /gadgets/{gadget_id}/run'] }],
    })
    expect(drift.uncovered).toEqual([])
  })

  test('every exclusion group needs a reason, and an entry is listed once', () => {
    const drift = coverageDrift(report, {
      excluded: [
        { reason: ' ', entries: ['GET /widgets'] },
        { reason: 'Not for the CLI', entries: ['GET /widgets', 'DELETE /widgets/{id}', 'POST /gadgets/{gadget_id}/run', 'POST /widgets body.color'] },
      ],
    })
    expect(drift.unexplained).toBe(1)
    expect(drift.duplicates).toEqual(['GET /widgets'])
    expect(describeCoverageDrift(drift)).toContain('no reason')
  })
})

describe('repository', () => {
  test('every API operation and input is covered by a command or excluded with a reason', () => {
    const failure = describeCoverageDrift(coverageDrift(repositoryCoverage(), readAllowlist()))
    expect(failure).toBeNull()
    // Scans every command source against the whole spec: seconds under a loaded full run.
  }, 30_000)

  test('cli-api-coverage.json is in canonical form', async () => {
    const path = fileURLToPath(new URL('../cli-api-coverage.json', import.meta.url))
    const text = await Bun.file(path).text()
    expect(text).toBe(formatAllowlist(readAllowlist(path)))
  })
})
