// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import {
  describeParameters,
  hasHiddenValue,
  humanizeKey,
  summaryTitle,
  upstreamFailureMessage,
} from './platform-write-view'

test('keys read as labels', () => {
  expect(humanizeKey('bucket_name')).toBe('Bucket name')
  expect(humanizeKey('force-path-style')).toBe('Force path style')
})

test('parameters become one readable row each, with credentials hidden', () => {
  const rows = describeParameters({
    name: 'backups',
    force_path_style: true,
    retention_period: 30,
    tags: ['a', 'b'],
    description: null,
    access_key_id: '***',
    secret_key: 'secret_ref:v1:abc',
  })
  expect(rows.map((r) => r.key)).toEqual([
    'name',
    'force_path_style',
    'retention_period',
    'tags',
    'access_key_id',
    'secret_key',
  ])
  expect(rows[1]?.value).toEqual({ kind: 'text', text: 'Yes' })
  expect(rows[2]?.value).toEqual({ kind: 'text', text: '30' })
  expect(rows[3]?.value).toEqual({ kind: 'text', text: 'a, b' })
  expect(rows[4]?.value).toEqual({ kind: 'hidden' })
  expect(rows[5]?.value).toEqual({ kind: 'hidden' })
  expect(hasHiddenValue(rows)).toBe(true)
})

test('a handle is never shown as text', () => {
  const rows = describeParameters({ secret_key: 'secret_ref:v1:abc' })
  expect(JSON.stringify(rows)).not.toContain('secret_ref')
})

test('nested objects are flattened and empty lists are named', () => {
  const rows = describeParameters({ retry: { count: 3 }, tags: [] })
  expect(rows[0]).toMatchObject({ key: 'retry.count', label: 'Count' })
  expect(rows[1]?.value).toEqual({ kind: 'text', text: 'None' })
})

test('non-objects have no rows', () => {
  expect(describeParameters(null)).toEqual([])
  expect(describeParameters('x')).toEqual([])
  expect(describeParameters([1])).toEqual([])
})

test('the summary loses its route suffix', () => {
  expect(
    summaryTitle('Create a new S3 source (POST /backups/s3-sources)')
  ).toBe('Create a new S3 source')
  expect(summaryTitle('')).toBeNull()
  expect(summaryTitle(undefined)).toBeNull()
})

test('a rejected change explains itself in one sentence', () => {
  const error =
    'Upstream call to operation \'create_backup_schedule\' returned HTTP 400: {"detail":"Invalid backup schedule: 0 3 * * *","title":"Validation Error"}'
  expect(upstreamFailureMessage(error)).toBe(
    'Validation Error: Invalid backup schedule: 0 3 * * *'
  )
  expect(upstreamFailureMessage('Connection refused')).toBe(
    'Connection refused'
  )
  expect(upstreamFailureMessage('returned HTTP 502: {not json')).toBe(
    'returned HTTP 502: {not json'
  )
})
