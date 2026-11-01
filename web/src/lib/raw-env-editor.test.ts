// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  mapWithConcurrency,
  parseEnvText,
  planRawEnvChanges,
  rawEditorVariables,
  serializeEnvText,
  type EnvEntry,
} from './raw-env-editor'

const production = { id: 1, name: 'production', is_preview: false }
const staging = { id: 2, name: 'staging', is_preview: false }
const preview = { id: 3, name: 'preview-feature', is_preview: true }

function variable(
  id: number,
  key: string,
  environments: { id: number; name: string }[],
  options: { include_in_preview?: boolean; is_secret?: boolean } = {}
) {
  return {
    id,
    key,
    environments: environments.map((env) => ({ ...env, main_url: '' })),
    include_in_preview: options.include_in_preview ?? false,
    is_secret: options.is_secret ?? false,
  }
}

describe('serializeEnvText / parseEnvText', () => {
  test('round-trips values that need quoting', () => {
    const entries: EnvEntry[] = [
      { key: 'PLAIN', value: 'https://api.example.com/v1?a=b' },
      { key: 'EMPTY', value: '' },
      {
        key: 'MULTILINE',
        value: '-----BEGIN KEY-----\nabc\n-----END KEY-----',
      },
      { key: 'QUOTES', value: `say "hi" and 'bye'` },
      { key: 'BACKSLASH_N', value: 'literal \\n, not a newline' },
      { key: 'HASH', value: 'pass #word' },
      { key: 'URL_FRAGMENT', value: 'https://example.com/#top' },
      { key: 'PADDED', value: '  spaced  ' },
      { key: 'TAB', value: 'a\tb' },
      { key: 'CRLF', value: 'a\r\nb' },
      { key: 'EQUALS', value: 'a=b=c' },
    ]

    const parsed = parseEnvText(serializeEnvText(entries))

    expect(parsed.errors).toEqual([])
    expect(parsed.entries).toEqual(entries)
  })

  test('leaves simple values unquoted so the text reads like a .env file', () => {
    expect(
      serializeEnvText([
        { key: 'A', value: '1' },
        { key: 'B', value: 'https://example.com' },
      ])
    ).toBe('A=1\nB=https://example.com\n')
  })

  test('accepts the dotenv conventions of a pasted file', () => {
    const parsed = parseEnvText(
      [
        '# a comment',
        '',
        'export TOKEN=abc',
        'INLINE=value # trailing comment',
        "SINGLE='literal \\n kept'",
        'DOUBLE="line one',
        'line two"',
        '  SPACED  =  trimmed  ',
      ].join('\r\n')
    )

    expect(parsed.errors).toEqual([])
    expect(parsed.entries).toEqual([
      { key: 'TOKEN', value: 'abc' },
      { key: 'INLINE', value: 'value' },
      { key: 'SINGLE', value: 'literal \\n kept' },
      { key: 'DOUBLE', value: 'line one\nline two' },
      { key: 'SPACED', value: 'trimmed' },
    ])
  })

  test('reports malformed lines instead of skipping them', () => {
    const parsed = parseEnvText(
      ['GOOD=1', 'not a variable', '=missing', 'BAD KEY=1', 'OPEN="x'].join(
        '\n'
      )
    )

    expect(parsed.entries).toEqual([{ key: 'GOOD', value: '1' }])
    expect(parsed.errors.map((error) => error.line)).toEqual([2, 3, 4, 5])
  })

  test('reports a duplicate key with the line it first appeared on', () => {
    const parsed = parseEnvText('A=1\nB=2\nA=3\n')

    expect(parsed.entries).toEqual([
      { key: 'A', value: '1' },
      { key: 'B', value: '2' },
    ])
    expect(parsed.errors).toEqual([
      { line: 3, message: 'A is already set on line 1' },
    ])
  })

  test('reports text after a closing quote', () => {
    const parsed = parseEnvText('A="x" y')

    expect(parsed.errors).toEqual([
      { line: 1, message: 'Unexpected text after the closing " of A' },
    ])
  })
})

describe('rawEditorVariables', () => {
  test('includes explicit bindings and preview inheritance, one per key', () => {
    const variables = [
      variable(1, 'SHARED', [production, staging]),
      variable(2, 'STAGING_ONLY', [staging]),
      variable(3, 'INHERITED', [production], { include_in_preview: true }),
      variable(4, 'OVERRIDE', [], { include_in_preview: true }),
      variable(5, 'OVERRIDE', [preview]),
    ]

    expect(
      rawEditorVariables(variables, production).map((v) => v.variable.id)
    ).toEqual([3, 1])
    expect(
      rawEditorVariables(variables, preview).map((v) => [
        v.variable.id,
        v.inherited,
      ])
    ).toEqual([
      [3, true],
      [5, false],
    ])
  })
})

describe('planRawEnvChanges', () => {
  const variables = [
    variable(1, 'ONLY_PROD', [production]),
    variable(2, 'SHARED', [production, staging]),
    variable(3, 'API_SECRET', [production], { is_secret: true }),
  ]
  const scoped = rawEditorVariables(variables, production)
  const revealed = new Map([
    [1, 'one'],
    [2, 'two'],
  ])
  const plan = (text: string) =>
    planRawEnvChanges({
      scoped,
      revealed,
      entries: parseEnvText(text).entries,
      environment: production,
    })

  test('saving the text it was opened with changes nothing', () => {
    const opened = serializeEnvText(
      scoped
        .filter(({ variable }) => !variable.is_secret)
        .map(({ variable }) => ({
          key: variable.key,
          value: revealed.get(variable.id) ?? '',
        }))
    )

    expect(plan(opened)).toEqual([])
  })

  test('creates new keys in the edited environment', () => {
    expect(plan('ONLY_PROD=one\nSHARED=two\nNEW=value')).toEqual([
      { type: 'create', key: 'NEW', value: 'value' },
    ])
  })

  test('says which other environments a shared value change reaches', () => {
    expect(plan('ONLY_PROD=one\nSHARED=changed')).toEqual([
      {
        type: 'update',
        variable: scoped.find((v) => v.variable.id === 2)!.variable,
        value: 'changed',
        alsoAffects: ['staging'],
        replacesSecret: false,
      },
    ])
  })

  test('deletes a removed variable bound only here and unbinds a shared one', () => {
    expect(plan('')).toEqual([
      { type: 'delete', variable: variables[0] },
      { type: 'unbind', variable: variables[1], remainingEnvironmentIds: [2] },
    ])
  })

  test('never touches a secret that is absent from the text', () => {
    const changes = plan('ONLY_PROD=one\nSHARED=two')

    expect(
      changes.some((change) => 'variable' in change && change.variable.id === 3)
    ).toBe(false)
  })

  test('rotates a secret whose key is typed, but refuses an empty one', () => {
    expect(plan('ONLY_PROD=one\nSHARED=two\nAPI_SECRET=rotated')).toEqual([
      {
        type: 'update',
        variable: variables[2],
        value: 'rotated',
        alsoAffects: [],
        replacesSecret: true,
      },
    ])
    expect(plan('ONLY_PROD=one\nSHARED=two\nAPI_SECRET=')[0]?.type).toBe(
      'blocked'
    )
  })

  test('never deletes a variable whose value was not loaded', () => {
    const changes = planRawEnvChanges({
      scoped,
      revealed: new Map([[2, 'two']]),
      entries: parseEnvText('SHARED=two').entries,
      environment: production,
    })

    expect(changes.map((change) => change.type)).toEqual(['blocked'])
  })

  test('blocks removing a preview-inherited variable from one preview', () => {
    const inherited = variable(9, 'INHERITED', [production], {
      include_in_preview: true,
    })
    const changes = planRawEnvChanges({
      scoped: rawEditorVariables([inherited], preview),
      revealed: new Map([[9, 'x']]),
      entries: [],
      environment: preview,
    })

    expect(changes.map((change) => change.type)).toEqual(['blocked'])
  })

  test('keeps preview inheritance when unbinding the last explicit environment', () => {
    const both = variable(7, 'BOTH', [production], { include_in_preview: true })
    const changes = planRawEnvChanges({
      scoped: rawEditorVariables([both], production),
      revealed: new Map([[7, 'x']]),
      entries: [],
      environment: production,
    })

    expect(changes).toEqual([
      { type: 'unbind', variable: both, remainingEnvironmentIds: [] },
    ])
  })
})

describe('mapWithConcurrency', () => {
  test('keeps results in input order', async () => {
    const results = await mapWithConcurrency([30, 10, 20], 2, async (ms) => {
      await new Promise((resolve) => setTimeout(resolve, ms))
      return ms * 2
    })

    expect(results).toEqual([60, 20, 40])
  })

  test('starts nothing new once one reveal is refused', async () => {
    const started: number[] = []
    const refusal = { title: 'Insufficient Permissions', status: 403 }

    const outcome = mapWithConcurrency(
      Array.from({ length: 20 }, (_, index) => index),
      4,
      async (index) => {
        started.push(index)
        if (index === 0) throw refusal
        await new Promise((resolve) => setTimeout(resolve, 5))
        return index
      }
    )

    await expect(outcome).rejects.toBe(refusal)
    await new Promise((resolve) => setTimeout(resolve, 50))
    // Only the reveals already in flight; the other 16 are never requested.
    expect(started).toEqual([0, 1, 2, 3])
  })
})
