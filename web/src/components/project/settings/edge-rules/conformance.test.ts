// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Runs the engine's conformance fixtures through the in-browser tester. The
// proxy's Rust engine runs the same files (crates/temps-edge-rules/tests),
// so "Test a request" in the console cannot disagree with the proxy.

import { describe, expect, test } from 'bun:test'
import { readdirSync, readFileSync } from 'node:fs'
import { join } from 'node:path'
import {
  groupFromApi,
  actionFromApi,
  groupToApi,
  sampleFromApi,
  type ApiAction,
  type ApiGroup,
  type ApiSampleRequest,
} from './api'
import { evaluateRules, type Decision, type RuleOutcome } from './evaluate'
import type { EdgeRule, RuleMode } from './types'

interface Fixture {
  description: string
  rules: Array<{
    id: number
    enabled?: boolean
    mode?: RuleMode
    environment_id?: number | null
    expression: ApiGroup
    action: ApiAction
  }>
  cases: Array<{
    name: string
    request: ApiSampleRequest
    decision: {
      kind: Decision['kind']
      rule_id?: number
      status?: number
      location?: string
    }
    trace?: RuleOutcome[]
  }>
}

const dir = join(
  import.meta.dir,
  '../../../../../../crates/temps-edge-rules/tests/conformance'
)
const files = readdirSync(dir)
  .filter((f) => f.endsWith('.json'))
  .sort()

test('the fixtures are present', () => {
  expect(files.length).toBeGreaterThanOrEqual(4)
})

for (const file of files) {
  const fixture = JSON.parse(readFileSync(join(dir, file), 'utf8')) as Fixture
  const rules: EdgeRule[] = fixture.rules.map((r) => ({
    id: String(r.id),
    revision: 1,
    name: `rule ${r.id}`,
    enabled: r.enabled ?? true,
    mode: r.mode ?? 'enforce',
    environmentId: r.environment_id ?? null,
    expression: groupFromApi(r.expression),
    action: actionFromApi(r.action),
    createdAt: '',
    updatedAt: '',
  }))

  describe(fixture.description, () => {
    for (const c of fixture.cases) {
      test(c.name, () => {
        const result = evaluateRules(rules, sampleFromApi(c.request))
        const d = result.decision
        const got = {
          kind: d.kind,
          rule_id: 'ruleId' in d ? Number(d.ruleId) : undefined,
          status: 'status' in d ? d.status : undefined,
          location: 'location' in d ? d.location : undefined,
        }
        expect(got).toEqual({
          kind: c.decision.kind,
          rule_id: c.decision.rule_id,
          status: c.decision.status,
          location: c.decision.location,
        })
        if (c.trace) {
          expect(result.trace.map((t) => t.outcome)).toEqual(c.trace)
        }
      })
    }
  })
}

test('API conversion round-trips list values and keys', () => {
  const api: ApiGroup = {
    id: 'root',
    combinator: 'all',
    children: [
      {
        kind: 'condition',
        id: 'c1',
        field: 'country',
        op: 'in',
        values: ['ES', 'PT'],
      },
      {
        kind: 'condition',
        id: 'c2',
        field: 'header',
        key: 'X-Api-Key',
        op: 'exists',
        values: [],
      },
    ],
  }
  const back = groupToApi(groupFromApi(api))
  expect(back.children?.[0]).toMatchObject({ values: ['ES', 'PT'] })
  expect(back.children?.[1]).toMatchObject({ key: 'X-Api-Key', values: [] })
})
