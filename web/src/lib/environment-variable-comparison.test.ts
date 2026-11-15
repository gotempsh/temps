// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type {
  EnvironmentResponse,
  EnvironmentVariableResponse,
} from '@/api/client/types.gen'
import {
  compareEnvironmentVariableKeys,
  groupVariableEnvironments,
  setEnvironmentSelection,
  toggleEnvironmentSelection,
  variableAppliesToEnvironment,
  orderEnvironments,
  orderVariableEnvironments,
} from './environment-variable-comparison'

const environment = (id: number, name: string, is_preview = false) =>
  ({ id, name, is_preview }) as EnvironmentResponse
const variable = (key: string, ids: number[], include_in_preview = false) =>
  ({
    key,
    environments: ids.map((id) => ({ id, name: `env-${id}` })),
    include_in_preview,
  }) as EnvironmentVariableResponse

describe('environment variable comparison', () => {
  test('puts production and other stable environments before previews', () => {
    const environments = [
      environment(3, 'branch-c', true),
      environment(2, 'staging'),
      environment(1, 'production'),
    ]
    expect(orderEnvironments(environments).map((env) => env.id)).toEqual([
      1, 2, 3,
    ])
    expect(
      orderVariableEnvironments(
        environments.map(({ id, name }) => ({ id, name, main_url: '' })),
        new Set([3])
      ).map((env) => env.id)
    ).toEqual([1, 2, 3])
  })

  test('compares keys by name and respects preview inheritance', () => {
    const variables = [
      variable('SHARED', [1, 2]),
      variable('PROD_ONLY', [1]),
      variable('PREVIEW_DEFAULT', [], true),
      variable('PROD_ONLY', [2]),
    ]
    expect(
      compareEnvironmentVariableKeys(
        variables,
        environment(2, 'staging'),
        environment(3, 'branch-c', true)
      )
    ).toEqual({
      missingInFirst: ['PREVIEW_DEFAULT'],
      missingInSecond: ['PROD_ONLY', 'SHARED'],
    })
  })
})

test('environment filtering includes explicit bindings and preview inheritance only', () => {
  const prod = environment(1, 'production')
  const preview = environment(3, 'preview', true)
  expect(variableAppliesToEnvironment(variable('PROD', [1]), prod)).toBe(true)
  expect(variableAppliesToEnvironment(variable('PROD', [1]), preview)).toBe(
    false
  )
  expect(
    variableAppliesToEnvironment(variable('PREVIEW', [], true), preview)
  ).toBe(true)
  expect(
    variableAppliesToEnvironment(variable('PREVIEW', [], true), prod)
  ).toBe(false)
  expect(variableAppliesToEnvironment(variable('UNBOUND', []), prod)).toBe(
    false
  )
  expect(variableAppliesToEnvironment(variable('UNBOUND', []), undefined)).toBe(
    true
  )
})

describe('variable environment grouping', () => {
  const binding = (id: number, name: string) => ({ id, name, main_url: '' })

  test('separates stable, preview and deleted bindings', () => {
    const groups = groupVariableEnvironments(
      [
        binding(4, 'removed-branch'),
        binding(3, 'branch-c'),
        binding(2, 'staging'),
        binding(1, 'production'),
      ],
      new Set([3]),
      new Set([1, 2, 3])
    )
    expect(groups.stable.map((env) => env.id)).toEqual([1, 2])
    expect(groups.previews.map((env) => env.id)).toEqual([3])
    expect(groups.deleted.map((env) => env.id)).toEqual([4])
  })

  test('classifies nothing as deleted while the environment list is unknown', () => {
    const groups = groupVariableEnvironments(
      [binding(4, 'removed-branch'), binding(1, 'production')],
      new Set(),
      undefined
    )
    expect(groups.stable.map((env) => env.id)).toEqual([1, 4])
    expect(groups.deleted).toEqual([])
  })
})

describe('environment picker selection', () => {
  test('toggles one environment', () => {
    expect(toggleEnvironmentSelection([1, 2], 3)).toEqual([1, 2, 3])
    expect(toggleEnvironmentSelection([1, 2, 3], 2)).toEqual([1, 3])
  })

  test('selects and clears a batch without touching the rest', () => {
    expect(setEnvironmentSelection([1], [2, 1, 3], true)).toEqual([1, 2, 3])
    expect(setEnvironmentSelection([1, 2, 5], [2, 3], false)).toEqual([1, 5])
  })

  test('keeps links the picker does not show when the operator edits', () => {
    // 9 is a link to an environment this page thinks is deleted (it may have
    // been restored since). Clearing or toggling what the picker shows must
    // never drop it: losing a variable's last link makes it apply everywhere.
    const linked = [1, 9]
    const shown = [1, 2, 3]
    expect(setEnvironmentSelection(linked, shown, false)).toEqual([9])
    expect(setEnvironmentSelection(linked, shown, true)).toEqual([1, 9, 2, 3])
    expect(toggleEnvironmentSelection(linked, 1)).toEqual([9])
  })
})
