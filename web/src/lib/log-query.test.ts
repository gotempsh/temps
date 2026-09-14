// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import { parseLogQuery, quoteLogValue } from './log-query'
const projects = [{ id: 1, name: 'Sample Store', slug: 'sample-store' }]
test('parses pasted filters and message text atomically', () => {
  expect(
    parseLogQuery(
      'level:error project:"Sample Store" env:Production timeout',
      projects
    )
  ).toEqual({
    patch: {
      level: 'ERROR',
      project_id: '1',
      source: 'application',
      env: 'Production',
      q: 'timeout',
    },
  })
  expect(parseLogQuery('node:7 deployment:42 source:application', [])).toEqual({
    patch: {
      node_id: '7',
      deploy_id: '42',
      source: 'application',
      q: undefined,
    },
  })
})
test('rejects invalid filters without partially applying the query', () => {
  for (const input of [
    'level:fatal',
    'project:missing',
    'node:-1',
    'deployment:1.5',
    'source:proxy',
    'env:',
    'env:"unfinished',
    'source:service project:1',
  ])
    expect(parseLogQuery(input, projects).error).toBeTruthy()
})
test('preserves literal messages and quoted environment values', () => {
  expect(parseLogQuery('request_id:abc failed', []).patch?.q).toBe(
    'request_id:abc failed'
  )
  const value = 'staging "east"'
  expect(parseLogQuery(`env:${quoteLogValue(value)}`, []).patch?.env).toBe(
    value
  )
  expect(parseLogQuery('source:service', []).patch).toEqual({
    source: 'service',
    project_id: undefined,
    q: undefined,
  })
  expect(parseLogQuery('', []).patch).toEqual({ q: undefined })
})
