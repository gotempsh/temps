import { describe, expect, test } from 'bun:test'
import type {
  EnvironmentResponse,
  EnvironmentVariableResponse,
} from '@/api/client/types.gen'
import {
  compareEnvironmentVariableKeys,
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
